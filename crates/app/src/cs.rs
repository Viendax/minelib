//! Règles compétitives de CS2 tenues par l'hôte d'une partie FPS en ligne.
//!
//! - Échauffement jusqu'à 10 joueurs (5 contre 5), puis 24 manches au plus : la première
//!   équipe à 13 gagne ; changement de côté après 12 manches. À 12-12, prolongations par
//!   blocs de 6 manches (3 de chaque côté) : premier à 4 manches dans le bloc, sinon un
//!   autre bloc.
//! - Chaque manche : 20 s de départ (immobiles, achats), 1 min 55 de jeu, 20 s d'achats
//!   après le départ ; bombe posée en 3,2 s sur un site, explosion au bout de 40 s,
//!   désamorçage en 10 s (5 s avec le kit).
//! - Argent : 800 $ au départ (12 500 $ par moitié de prolongation), 16 000 $ au plus ;
//!   victoire 3 250 $ (3 500 $ par la bombe), défaite 1 400 $ + 500 $ par défaite d'affilée
//!   (3 400 $ au plus, compteur diminué d'une défaite à chaque victoire, comme CS2), 800 $
//!   de plus aux T quand ils ont posé ; primes d'élimination selon l'arme ; 300 $ pour
//!   poser ou désamorcer. Les T encore en vie quand le temps est écoulé ne touchent rien.
//! - Tir sur un coéquipier : un tiers des dégâts ; élimination d'un coéquipier : −300 $.
//! - Votes d'équipe (F1 oui, F2 non, 30 s) : exclure un coéquipier (tous les autres
//!   coéquipiers d'accord), pause tactique (la majorité ; 3 par équipe, 30 s, prise au
//!   départ de la manche suivante). Un joueur déconnecté : pause technique au départ de la
//!   manche suivante, jusqu'à son retour (5 minutes au plus).

use std::collections::BTreeMap;

use glam::Vec3;
use minelib_net::game::{
    bomb, phase, reason, GroundGun, MatchHud, MatchSummary, ScoreRow, SummaryPlayer, VoteHud, Zone,
    CT, DROP_BOMB, ITEM_HELMET, ITEM_KEVLAR, ITEM_KIT, ITEM_NADE, PAUSE_TACTICAL, PAUSE_TECHNICAL,
    T, VOTE_KICK, VOTE_PAUSE, VOTE_SURRENDER,
};

use crate::weapons::WEAPONS;

const HALF: u32 = 12;
const WIN: u8 = 13;
pub const FREEZE_S: f32 = 20.0;
pub const ROUND_S: f32 = 115.0;
pub const BUY_S: f32 = 20.0;
pub const BOMB_S: f32 = 40.0;
pub const PLANT_S: f32 = 3.2;
pub const DEFUSE_S: f32 = 10.0;
pub const DEFUSE_KIT_S: f32 = 5.0;
const ROUND_END_S: f32 = 7.0;
const HALFTIME_S: f32 = 15.0;
/// Compte à rebours une fois les 10 joueurs là.
const START_S: f32 = 10.0;
const WARMUP_RESPAWN_S: f32 = 2.0;
pub const START_MONEY: u32 = 800;
const OT_MONEY: u32 = 12_500;
pub const MAX_MONEY: u32 = 16_000;
/// En vie dans une manche en cours sans toucher une commande pendant ce temps : sorti de
/// la partie.
pub const AFK_S: f32 = 120.0;
/// Manches jouées sans un coéquipier avant que le reste de l'équipe soit compensé.
pub const ABSENT_ROUNDS: u8 = 2;
/// Compensation par joueur absent et par manche, partagée entre les coéquipiers présents.
pub const ABSENT_COMP: u32 = 2_500;
const WIN_ELIM: u32 = 3_250;
const WIN_BOMB: u32 = 3_500;
const LOSS_BASE: u32 = 1_400;
const LOSS_STEP: u32 = 500;
const PLANT_TEAM_BONUS: u32 = 800;
const OBJECTIVE_REWARD: u32 = 300;
const TEAMKILL_PENALTY: u32 = 300;
/// Coéquipiers tués avant d'être exclu de la partie.
pub const TEAMKILL_KICK: u16 = 3;
/// Durée d'un vote (s).
pub const VOTE_S: f32 = 30.0;
/// Pauses tactiques par équipe et durée (s), comme dans CS2.
pub const TIMEOUTS: u8 = 3;
pub const TIMEOUT_S: f32 = 30.0;
/// Pause technique (joueur déconnecté) : au plus 5 minutes.
pub const TECH_PAUSE_S: f32 = 300.0;
pub const KIT_PRICE: u32 = 400;
pub const KEVLAR_PRICE: u32 = 650;
pub const HELMET_PRICE: u32 = 1_000;
/// Distance (blocs) pour ramasser la bombe, et pour la désamorcer.
const PICKUP: f32 = 1.2;
/// Distance au sol au plus d'un objet lâché (lancer en cloche d'environ 4 blocs).
pub const MAX_TOSS: f32 = 6.0;
/// Hauteur de chute au plus d'un objet lâché (depuis un toit, au-dessus du vide).
const MAX_TOSS_FALL: f32 = 40.0;
const DEFUSE_REACH: f32 = 1.8;
/// Rayon de l'explosion de la bombe (écart type de la courbe de dégâts de CS, en blocs).
const BOMB_SIGMA: f32 = 14.8;
const BOMB_DAMAGE: f32 = 500.0;

/// Codes d'« arme » pour le fil des éliminations, en plus des indices d'armes.
pub const W_KNIFE: u8 = 255;
pub const W_HE: u8 = 250;
pub const W_FIRE: u8 = 251;
pub const W_BOMB: u8 = 252;
pub const W_WORLD: u8 = 253;

/// Nature d'un dégât.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hurt {
    Bullet { weapon: u8, head: bool, legs: bool },
    Knife,
    Grenade,
    Fire,
    Bomb,
    World,
}

impl Hurt {
    fn code(self) -> u8 {
        match self {
            Hurt::Bullet { weapon, .. } => weapon,
            Hurt::Knife => W_KNIFE,
            Hurt::Grenade => W_HE,
            Hurt::Fire => W_FIRE,
            Hurt::Bomb => W_BOMB,
            Hurt::World => W_WORLD,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct MPlayer {
    /// Lobby (0 : seul) : les joueurs d'un même lobby sont dans la même équipe.
    pub party: u64,
    /// Couleur dans l'équipe (0 à 4) et couleur préférée (255 : aucune).
    pub color: u8,
    pub pref: u8,
    /// Coéquipiers tués dans cette partie (3 : exclu).
    pub teamkills: u16,
    /// Exclu par un vote de son équipe.
    #[serde(default)]
    pub kicked: bool,
    pub pseudo: String,
    pub team: u8,
    pub start_team: u8,
    pub present: bool,
    pub alive: bool,
    pub hp: u32,
    pub money: u32,
    pub armor: u8,
    pub helmet: bool,
    pub kit: bool,
    pub kills: u16,
    pub deaths: u16,
    pub assists: u16,
    pub headshots: u16,
    pub damage: u32,
    pub mvps: u16,
    pub score: i32,
    pub rounds: u8,
    round_kills: u16,
    round_damage: u32,
    /// Dégâts infligés à chaque adversaire cette manche (passes décisives).
    dealt: BTreeMap<u64, u32>,
    dead_for: f32,
    /// Touches maintenues : poser, désamorcer.
    pub plant_key: bool,
    pub defuse_key: bool,
    /// Armes portées (principale, secondaire ; 255 : aucune) et grenades par type. C'est
    /// l'hôte qui fait foi : on ne tire qu'avec une arme qu'on porte.
    pub guns: (u8, u8),
    pub nades: [u8; 5],
    /// Cadence de tir : tirs encore permis (se recharge à la cadence de l'arme) et pas
    /// de la dernière recharge.
    shots_left: f32,
    shots_tick: u32,
    /// Il faudra repartir avec le pistolet de départ (mort pendant la manche).
    pub reset: bool,
    /// Temps passé en vie dans une manche en cours sans toucher une seule commande (s),
    /// et empreinte de ses dernières entrées (pour voir qu'elles changent).
    #[serde(default)]
    pub idle: f32,
    #[serde(default)]
    last_input: u64,
    /// Manches commencées sans lui depuis qu'il est parti.
    #[serde(default)]
    pub missed: u8,
}

impl MPlayer {
    /// Exclu de la partie (coéquipiers tués, vote de son équipe) : ne revient pas.
    pub fn excluded(&self) -> bool {
        self.kicked || self.teamkills >= TEAMKILL_KICK
    }
}

/// Où se trouve un joueur (fourni par l'hôte à chaque pas).
#[derive(Clone, Copy, Debug)]
pub struct Place {
    pub feet: Vec3,
    pub on_ground: bool,
    pub speed: f32,
}

/// Ce que l'hôte doit faire après un pas des règles.
#[derive(Clone, Debug, PartialEq)]
pub enum Ev {
    /// Faire réapparaître ce joueur dans la zone de son équipe.
    Spawn {
        id: u64,
        reset: bool,
    },
    /// Un joueur est mort (fil des éliminations : tueur, arme, tête).
    Killed {
        victim: u64,
        killer: Option<u64>,
        weapon: u8,
        head: bool,
    },
    RoundEnd {
        winner: u8,
        reason: u8,
    },
    Say(String),
    /// Les points de vie d'un joueur ont changé.
    Hp(u64),
    GameOver(MatchSummary),
    /// Tout le monde repart de zéro (début de la partie).
    Started,
    /// Nouvelle manche (les armes au sol disparaissent).
    NewRound,
    /// Trois coéquipiers tués : ce joueur est exclu de la partie.
    TeamKillKick(u64),
    /// Exclu par un vote de son équipe.
    VoteKick(u64),
    /// Sorti de la partie pour inactivité (2 minutes en vie sans rien toucher) : il peut
    /// revenir, et il est puni comme pour une déconnexion.
    AfkKick(u64),
    /// Pause commencée (true) ou finie : le relais ne compte pas ce temps dans les
    /// absences.
    Paused(bool),
}

/// Vote d'une équipe en cours.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Vote {
    pub kind: u8,
    pub team: u8,
    pub caller: u64,
    pub target: u64,
    pub yes: Vec<u64>,
    pub no: Vec<u64>,
    pub left: f32,
}

/// Pause en cours.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Pause {
    pub kind: u8,
    pub left: f32,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Match {
    pub players: BTreeMap<u64, MPlayer>,
    pub phase: u8,
    pub time: f32,
    /// Manches jouées.
    pub played: u32,
    /// Manches gagnées par équipe de départ (0 = a commencé CT).
    pub wins: [u8; 2],
    /// Les équipes ont changé de côté (nombre impair de changements).
    pub swapped: bool,
    loss_streak: [u32; 2],
    /// Vainqueur (équipe de départ) et raison de chaque manche.
    pub history: Vec<(u8, u8)>,
    pub bomb: u8,
    pub carrier: Option<u64>,
    pub bomb_pos: Vec3,
    /// Bombe lâchée : celui qui l'a lâchée ne peut la reprendre avant ce délai (s).
    bomb_lock: Option<(u64, f32)>,
    pub site: u8,
    planter: Option<u64>,
    plant_progress: f32,
    defuser: Option<u64>,
    defuse_progress: f32,
    /// Temps de jeu écoulé dans la manche (fin des achats).
    live_for: f32,
    pub map: minelib_net::game::FpsMap,
    pub map_sig: [u8; 32],
    rng: u32,
    /// Manche gagnée par les T après avoir posé (prime aux T).
    planted_this_round: bool,
    /// Compte à rebours du début lancé (10 joueurs).
    countdown: bool,
    /// Carte sans zones de départ : centre des départs CT et T (achats autour).
    pub fallback_spawns: Option<[Vec3; 2]>,
    /// Armes au sol.
    pub ground: Vec<GroundGun>,
    next_gun: u32,
    /// Pas de règles écoulés (cadences de tir).
    ticks: u32,
    /// Portes et trappes changées pendant la partie : case → (bloc actuel, bloc d'origine).
    pub doors: BTreeMap<[i32; 3], [u16; 2]>,
    /// Votes en cours (un par équipe au plus).
    #[serde(default)]
    pub votes: Vec<Vote>,
    /// Pauses tactiques prises, par équipe de départ (0 = a commencé CT).
    #[serde(default)]
    pub timeouts: [u8; 2],
    /// Pauses tactiques votées, à prendre au prochain départ (équipes de départ).
    #[serde(default)]
    pub pending_timeouts: Vec<u8>,
    /// Un joueur s'est déconnecté : pause technique au prochain départ.
    #[serde(default)]
    pub tech_pending: bool,
    /// Compensations versées au départ de cette manche : (joueur absent, son équipe, part
    /// de chaque coéquipier). S'il revient avant la fin du temps de départ, elles sont
    /// reprises.
    #[serde(default)]
    comp: Vec<(u64, u8, u32)>,
    /// Équipe (de départ : 0 a commencé CT) qui a abandonné.
    #[serde(default)]
    pub surrendered: Option<u8>,
    #[serde(default)]
    pub pause: Option<Pause>,
}

/// Couleurs de joueurs d'une équipe (voir `multiplayer::PLAYER_COLORS`).
const COLORS: usize = crate::multiplayer::PLAYER_COLORS.len();

/// Tirs d'avance permis (les messages arrivent parfois groupés).
const BURST: f32 = 3.0;

/// Type de grenade d'un objet au sol (`ITEM_NADE` + type), sinon `None` (arme).
pub fn nade_code(weapon: u8) -> Option<usize> {
    let n = (weapon as u16).checked_sub(ITEM_NADE)? as usize;
    (n < 5).then_some(n)
}

/// Pistolet de départ d'une équipe (Glock-18 pour les T, USP-S pour les CT).
pub fn starter(team: u8) -> u8 {
    let name = if team == T { "Glock-18" } else { "USP-S" };
    WEAPONS
        .iter()
        .position(|w| w.name == name)
        .map_or(255, |i| i as u8)
}

fn other(team: u8) -> u8 {
    1 - team
}

impl Match {
    /// Un joueur change d'identifiant (nouvel hôte : il devient 0) ; `None` : retiré.
    pub fn rename(&mut self, from: u64, to: Option<u64>) {
        if let Some(p) = self.players.remove(&from) {
            if let Some(to) = to {
                self.players.insert(to, p);
            }
        }
        let fix = |v: &mut Option<u64>| {
            if *v == Some(from) {
                *v = to;
            }
        };
        fix(&mut self.carrier);
        fix(&mut self.planter);
        fix(&mut self.defuser);
        self.votes
            .retain(|v| v.caller != from && v.target != from || to.is_some());
        for v in &mut self.votes {
            if let Some(to) = to {
                for id in [&mut v.caller, &mut v.target]
                    .into_iter()
                    .chain(v.yes.iter_mut())
                    .chain(v.no.iter_mut())
                {
                    if *id == from {
                        *id = to;
                    }
                }
            } else {
                v.yes.retain(|&i| i != from);
                v.no.retain(|&i| i != from);
            }
        }
        if self.carrier.is_none() && self.bomb == bomb::CARRIED {
            self.bomb = bomb::DROPPED;
        }
        for p in self.players.values_mut() {
            if let Some(d) = p.dealt.remove(&from) {
                if let Some(to) = to {
                    p.dealt.insert(to, d);
                }
            }
        }
    }

    pub fn new(map: minelib_net::game::FpsMap, map_sig: [u8; 32], seed: u32) -> Self {
        Self {
            players: BTreeMap::new(),
            phase: phase::WARMUP,
            time: 0.0,
            played: 0,
            wins: [0, 0],
            swapped: false,
            loss_streak: [0, 0],
            history: Vec::new(),
            bomb: bomb::NONE,
            carrier: None,
            bomb_pos: Vec3::ZERO,
            bomb_lock: None,
            site: 0,
            planter: None,
            plant_progress: 0.0,
            defuser: None,
            defuse_progress: 0.0,
            live_for: 0.0,
            doors: BTreeMap::new(),
            map,
            map_sig,
            rng: seed | 1,
            planted_this_round: false,
            countdown: false,
            fallback_spawns: None,
            ground: Vec::new(),
            next_gun: 1,
            ticks: 0,
            votes: Vec::new(),
            timeouts: [0, 0],
            pending_timeouts: Vec::new(),
            tech_pending: false,
            comp: Vec::new(),
            surrendered: None,
            pause: None,
        }
    }

    /// Joueurs par équipe (5 pour une carte 5 contre 5).
    pub fn team_max(&self) -> usize {
        (self.map.players() / 2).max(1) as usize
    }

    /// Réapparition : `reset` (mort, changement de côté) remet le pistolet de départ.
    fn spawn(&mut self, id: u64, reset: bool) -> Ev {
        if reset {
            if let Some(p) = self.players.get_mut(&id) {
                p.guns = (255, starter(p.team));
                p.nades = [0; 5];
            }
        }
        Ev::Spawn { id, reset }
    }

    pub fn has_gun(&self, id: u64, weapon: u8) -> bool {
        weapon != 255
            && self
                .players
                .get(&id)
                .is_some_and(|p| p.guns.0 == weapon || p.guns.1 == weapon)
    }

    /// Un tir de `id` avec `weapon` (255 / 254 : couteau) de `rays` rayons : permis s'il
    /// porte l'arme, qu'elle tire autant de plombs, et à sa cadence (avec un peu d'avance,
    /// les messages arrivant parfois groupés). Le tir est alors compté.
    pub fn shoot(&mut self, id: u64, weapon: u8, rays: usize) -> bool {
        let knife = weapon >= 254;
        let (rpm, pellets) = if knife {
            (150.0, 1)
        } else {
            match WEAPONS.get(weapon as usize) {
                Some(w) if self.has_gun(id, weapon) => (w.rpm, w.pellets.max(1) as usize),
                _ => return false,
            }
        };
        if rays == 0 || rays > pellets {
            return false;
        }
        let now = self.ticks;
        let Some(p) = self.players.get_mut(&id) else {
            return false;
        };
        let gained = now.wrapping_sub(p.shots_tick) as f32 * rpm / (60.0 * 256.0);
        p.shots_left = (p.shots_left + gained).min(BURST);
        p.shots_tick = now;
        if p.shots_left < 1.0 {
            return false;
        }
        p.shots_left -= 1.0;
        true
    }

    /// Lancer d'une grenade (code réseau : type, ou leurre qui imite une arme) : il faut
    /// en porter une de ce type (elle est retirée).
    pub fn throw(&mut self, id: u64, nade: u8) -> bool {
        let Some((kind, _)) = crate::equip::nade_from_net(nade) else {
            return false;
        };
        match self
            .players
            .get_mut(&id)
            .and_then(|p| p.nades.get_mut(kind.index()))
        {
            Some(n) if *n > 0 => {
                *n -= 1;
                true
            }
            _ => false,
        }
    }

    /// Pose au sol une arme que le joueur porte (celle d'un mort, ou l'ancienne lors d'un
    /// échange) : elle tombe à ses pieds.
    pub fn drop_gun(&mut self, id: u64, weapon: u8, at: Vec3) -> bool {
        self.toss(id, weapon, at, at)
    }

    /// Lâche (touche G) une arme, une grenade ou la bombe (`DROP_BOMB`) lancée en cloche
    /// depuis `from` (les pieds du joueur) jusqu'à `at`, le point de chute que le joueur a
    /// calculé : refusé s'il est trop loin pour un lancer (triche).
    pub fn toss(&mut self, id: u64, weapon: u8, from: Vec3, at: Vec3) -> bool {
        let d = at - from;
        let flat = Vec3::new(d.x, 0.0, d.z).length();
        if !at.is_finite() || flat > MAX_TOSS || d.y > 3.0 || d.y < -MAX_TOSS_FALL {
            return false;
        }
        if weapon == DROP_BOMB {
            if self.carrier != Some(id) || self.bomb != bomb::CARRIED {
                return false;
            }
            self.carrier = None;
            self.bomb = bomb::DROPPED;
            self.bomb_pos = at;
            // Celui qui la lâche ne la reprend pas aussitôt (elle peut retomber à ses pieds
            // contre un mur).
            self.bomb_lock = Some((id, 1.0));
            return true;
        }
        // Grenade (`ITEM_NADE` + type) : une de moins.
        if let Some(n) = nade_code(weapon) {
            match self.players.get_mut(&id) {
                Some(p) if p.nades[n] > 0 => p.nades[n] -= 1,
                _ => return false,
            }
        } else {
            if WEAPONS.get(weapon as usize).is_none() || !self.has_gun(id, weapon) {
                return false;
            }
            let p = self.players.get_mut(&id).expect("joueur");
            if p.guns.0 == weapon {
                p.guns.0 = 255;
            } else {
                p.guns.1 = 255;
            }
        }
        if self.ground.len() >= 48 {
            self.ground.remove(0);
        }
        self.ground.push(GroundGun {
            id: self.next_gun,
            weapon,
            pos: (at + Vec3::Y * 0.05).to_array(),
            from: (from + Vec3::Y * 1.3).to_array(),
        });
        if from == at {
            self.ground.last_mut().expect("posée").from = (at + Vec3::Y * 0.05).to_array();
        }
        self.next_gun += 1;
        true
    }

    /// Ramasse l'arme au sol `gun` : vivant, à portée ; celle du même emplacement est
    /// lâchée à la place (échange, comme dans CS). Renvoie l'arme prise.
    pub fn pickup(&mut self, id: u64, gun: u32, feet: Vec3) -> Option<u8> {
        if !self.players.get(&id).is_some_and(|p| p.alive) {
            return None;
        }
        let k = self
            .ground
            .iter()
            .position(|g| g.id == gun && (Vec3::from(g.pos) - feet).length() < 2.0)?;
        // Grenade : prise s'il reste de la place (mêmes limites qu'à l'achat).
        if let Some(n) = nade_code(self.ground[k].weapon) {
            let kind = crate::equip::Nade::ALL[n];
            let p = self.players.get_mut(&id)?;
            let total: u8 = p.nades.iter().sum();
            if p.nades[n] >= kind.max() || total >= crate::equip::MAX_NADES {
                return None;
            }
            p.nades[n] += 1;
            return Some(self.ground.remove(k).weapon);
        }
        let g = self.ground.remove(k);
        let primary = WEAPONS[g.weapon as usize].primary();
        let lo = self.players[&id].guns;
        let old = if primary { lo.0 } else { lo.1 };
        if old != 255 {
            self.drop_gun(id, old, feet);
        }
        let p = self.players.get_mut(&id).expect("joueur");
        if primary {
            p.guns.0 = g.weapon;
        } else {
            p.guns.1 = g.weapon;
        }
        Some(g.weapon)
    }

    /// Un joueur vient de mourir à `at` : sa meilleure arme tombe, il perd le reste.
    pub fn dropped_dead(&mut self, id: u64, at: Option<Vec3>) {
        let Some(lo) = self.players.get(&id).map(|p| p.guns) else {
            return;
        };
        let best = if lo.0 != 255 { lo.0 } else { lo.1 };
        if let (Some(at), true) = (at, best != 255) {
            self.drop_gun(id, best, at);
        }
        if let Some(p) = self.players.get_mut(&id) {
            p.guns = (255, 255);
            p.nades = [0; 5];
        }
    }

    fn rand(&mut self) -> u32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        self.rng
    }

    fn count(&self, team: u8) -> usize {
        self.players
            .values()
            .filter(|p| p.present && p.team == team)
            .count()
    }

    fn alive(&self, team: u8) -> usize {
        self.players
            .values()
            .filter(|p| p.present && p.alive && p.team == team)
            .count()
    }

    pub fn present(&self) -> usize {
        self.players.values().filter(|p| p.present).count()
    }

    pub fn started(&self) -> bool {
        self.phase != phase::WARMUP
    }

    /// Nom montré aux joueurs : pendant l'échauffement (tant que tout le monde n'est pas
    /// là), « Joueur n » : personne ne sait avec ou contre qui il va jouer.
    pub fn shown(&self, id: u64) -> String {
        let Some(p) = self.players.get(&id) else {
            return String::new();
        };
        if self.started() {
            return p.pseudo.clone();
        }
        let n = self.players.keys().position(|&k| k == id).unwrap_or(0) + 1;
        format!("Joueur {n}")
    }

    /// Un joueur arrive : CT ou T au hasard, 5 par équipe au plus.
    pub fn join(&mut self, id: u64, pseudo: &str) -> Vec<Ev> {
        self.join_party(id, pseudo, 0)
    }

    /// Un joueur arrive avec son lobby (`party`, 0 : seul) : avec ses amis déjà là.
    pub fn join_party(&mut self, id: u64, pseudo: &str, party: u64) -> Vec<Ev> {
        // Retour d'un joueur parti en pleine partie : il retrouve sa place (équipe, argent,
        // statistiques) et revient à la manche suivante. Exclu (coéquipiers tués) : non.
        let back = self
            .players
            .iter()
            .find(|(_, p)| !p.present && p.pseudo.eq_ignore_ascii_case(pseudo))
            .map(|(&k, _)| k);
        if let (true, Some(old)) = (self.started(), back) {
            let mut p = self.players.remove(&old).expect("joueur");
            if p.teamkills >= TEAMKILL_KICK {
                self.players.insert(id, p);
                return vec![Ev::TeamKillKick(id)];
            }
            if p.kicked {
                self.players.insert(id, p);
                return vec![Ev::VoteKick(id)];
            }
            p.present = true;
            p.alive = false;
            p.reset = true;
            p.idle = 0.0;
            p.missed = 0;
            let name = p.pseudo.clone();
            let team = p.team;
            self.players.insert(id, p);
            let mut ev = vec![Ev::Say(format!("{name} est de retour."))];
            // Revenu pendant le temps de départ : la compensation versée pour son absence
            // à cette manche est reprise tout de suite (ce qu'il en reste). Revenu en
            // pleine manche : elle reste, et il n'y en aura plus à la suivante.
            let mine: u32 = self.comp.iter().filter(|c| c.0 == old).map(|c| c.2).sum();
            self.comp.retain(|c| c.0 != old);
            if self.phase == phase::FREEZE && mine > 0 {
                for q in self.players.values_mut() {
                    if q.present && q.team == team && q.pseudo != name {
                        q.money = q.money.saturating_sub(mine);
                    }
                }
                ev.push(Ev::Say(format!(
                    "Compensation de la manche reprise (-${mine} par coéquipier)."
                )));
            }
            return ev;
        }
        let (ct, t) = (self.count(CT), self.count(T));
        let max = self.team_max();
        let friends = (party != 0)
            .then(|| {
                self.players
                    .values()
                    .find(|p| p.present && p.party == party)
                    .map(|p| p.team)
            })
            .flatten()
            .filter(|&team| self.count(team) < max);
        let team = if let Some(team) = friends {
            team
        } else if ct >= max {
            T
        } else if t >= max {
            CT
        } else if ct == t {
            if self.rand() & 1 == 0 {
                CT
            } else {
                T
            }
        } else if ct < t {
            CT
        } else {
            T
        };
        let money = if self.started() {
            START_MONEY
        } else {
            MAX_MONEY
        };
        self.players.insert(
            id,
            MPlayer {
                party,
                color: 0,
                pref: 255,
                teamkills: 0,
                kicked: false,
                pseudo: pseudo.to_string(),
                team,
                start_team: if self.swapped { other(team) } else { team },
                present: true,
                alive: !self.started(),
                hp: 100,
                money,
                armor: 0,
                helmet: false,
                kit: false,
                kills: 0,
                deaths: 0,
                assists: 0,
                headshots: 0,
                damage: 0,
                mvps: 0,
                score: 0,
                rounds: 0,
                round_kills: 0,
                round_damage: 0,
                dealt: BTreeMap::new(),
                dead_for: 0.0,
                plant_key: false,
                defuse_key: false,
                guns: (255, starter(team)),
                nades: [0; 5],
                shots_left: BURST,
                shots_tick: 0,
                reset: true,
                idle: 0.0,
                last_input: 0,
                missed: 0,
            },
        );
        self.assign_colors(team);
        let side = if team == CT { "CT" } else { "T" };
        let mut ev = vec![Ev::Say(format!("{} rejoint les {side}.", self.shown(id)))];
        if !self.started() {
            ev.push(self.spawn(id, true));
        }
        ev
    }

    /// Un joueur part (il reste au résultat s'il a commencé la partie).
    pub fn leave(&mut self, id: u64, at: Option<Vec3>) {
        if self.carrier == Some(id) {
            self.carrier = None;
            self.bomb = bomb::DROPPED;
            if let Some(p) = at {
                self.bomb_pos = p;
            }
        }
        if !self.started() {
            self.players.remove(&id);
        } else if let Some(p) = self.players.get_mut(&id) {
            p.present = false;
            p.alive = false;
            // Déconnecté (pas exclu) : pause technique au prochain départ.
            if !p.excluded() && self.phase != phase::GAME_OVER {
                self.tech_pending = true;
            }
        }
        for v in &mut self.votes {
            v.yes.retain(|&i| i != id);
            v.no.retain(|&i| i != id);
        }
        self.votes.retain(|v| v.caller != id && v.target != id);
    }

    /// Changer d'équipe : refusé. Les équipes sont faites à l'entrée (matchmaking, lobby
    /// dans la même équipe) et ne bougent plus, même pendant l'échauffement.
    pub fn switch_team(&mut self, _id: u64, _team: u8) -> Option<Ev> {
        None
    }

    /// Couleur préférée d'un joueur (gardée si personne d'autre ne l'a dans l'équipe).
    pub fn set_color(&mut self, id: u64, pref: u8) {
        let Some(p) = self.players.get_mut(&id) else {
            return;
        };
        p.pref = if (pref as usize) < COLORS { pref } else { 255 };
        let team = p.team;
        self.assign_colors(team);
    }

    /// Couleurs d'une équipe de cette carte : les N premières à N contre N.
    fn team_colors(&self) -> usize {
        (self.map.players() as usize / 2).clamp(1, COLORS)
    }

    /// Couleurs d'une équipe : chacun la sienne ; les préférences d'abord (le premier
    /// arrivé garde la sienne), les autres prennent les couleurs libres dans l'ordre.
    fn assign_colors(&mut self, team: u8) {
        let ids: Vec<u64> = self
            .players
            .iter()
            .filter(|(_, p)| p.team == team && p.present)
            .map(|(&k, _)| k)
            .collect();
        let n = self.team_colors();
        let mut taken = [false; COLORS];
        let mut rest = Vec::new();
        for &k in &ids {
            let pref = self.players[&k].pref;
            if (pref as usize) < n && !taken[pref as usize] {
                taken[pref as usize] = true;
                self.players.get_mut(&k).expect("joueur").color = pref;
            } else {
                rest.push(k);
            }
        }
        for k in rest {
            let c = taken[..n].iter().position(|t| !t).unwrap_or(0);
            taken[c] = true;
            self.players.get_mut(&k).expect("joueur").color = c as u8;
        }
    }

    /// Côté actuel d'une équipe de départ.
    fn start_of(&self, side: u8) -> u8 {
        if self.swapped {
            other(side)
        } else {
            side
        }
    }

    /// Manches gagnées par côté actuel [CT, T].
    pub fn score_sides(&self) -> [u8; 2] {
        [
            self.wins[self.start_of(CT) as usize],
            self.wins[self.start_of(T) as usize],
        ]
    }

    /// Le joueur peut-il acheter maintenant, là où il est ?
    pub fn can_buy(&self, id: u64, at: Option<Vec3>) -> bool {
        let Some(p) = self.players.get(&id) else {
            return false;
        };
        let time_ok = match self.phase {
            phase::WARMUP | phase::FREEZE => true,
            phase::LIVE => self.live_for < BUY_S,
            _ => false,
        };
        if !time_ok || !p.alive {
            return false;
        }
        let zone = if p.team == CT {
            self.map.ct
        } else {
            self.map.t
        };
        match (zone, at) {
            (Some(z), Some(f)) => {
                let grown = Zone {
                    min: [z.min[0] - 3, z.min[1] - 3, z.min[2] - 3],
                    max: [z.max[0] + 3, z.max[1] + 4, z.max[2] + 3],
                };
                grown.contains(f.to_array())
            }
            // Carte sans zones : autour du départ de son camp.
            (None, Some(f)) => match self.fallback_spawns {
                Some(b) => {
                    let c = b[p.team.min(1) as usize];
                    Vec3::new(f.x - c.x, 0.0, f.z - c.z).length() < 8.0
                }
                None => true,
            },
            (_, None) => true,
        }
    }

    /// Prix d'un article pour ce joueur, s'il peut l'acheter (camp, déjà porté).
    pub fn price(&self, id: u64, item: u16) -> Option<u32> {
        let p = self.players.get(&id)?;
        match item {
            ITEM_KIT => (p.team == CT && !p.kit).then_some(KIT_PRICE),
            ITEM_KEVLAR => (p.armor < 100).then_some(KEVLAR_PRICE),
            ITEM_HELMET => {
                if p.helmet && p.armor >= 100 {
                    None
                } else if p.armor >= 100 {
                    Some(HELMET_PRICE - KEVLAR_PRICE)
                } else {
                    Some(HELMET_PRICE)
                }
            }
            n if (ITEM_NADE..ITEM_NADE + 5).contains(&n) => {
                let kind = crate::equip::Nade::ALL[(n - ITEM_NADE) as usize];
                let total: u8 = p.nades.iter().sum();
                (p.nades[kind.index()] < kind.max() && total < crate::equip::MAX_NADES)
                    .then(|| kind.price())
            }
            w => {
                let def = WEAPONS.get(w as usize)?;
                if def.side().is_some_and(|s| s != p.team) {
                    return None;
                }
                Some(def.price)
            }
        }
    }

    /// Achat : renvoie vrai s'il est accepté (argent retiré, équipement noté).
    pub fn buy(&mut self, id: u64, item: u16, at: Option<Vec3>) -> Result<(), String> {
        if !self.can_buy(id, at) {
            return Err("Achats impossibles ici ou maintenant.".into());
        }
        let Some(price) = self.price(id, item) else {
            return Err("Article indisponible pour ton camp.".into());
        };
        let warmup = !self.started();
        let p = self.players.get_mut(&id).expect("joueur");
        if !warmup && p.money < price {
            return Err(format!("Pas assez d'argent ({} $ / {price} $).", p.money));
        }
        if !warmup {
            p.money -= price;
        }
        match item {
            ITEM_KIT => p.kit = true,
            ITEM_KEVLAR => p.armor = 100,
            ITEM_HELMET => {
                p.armor = 100;
                p.helmet = true;
            }
            n if (ITEM_NADE..ITEM_NADE + 5).contains(&n) => p.nades[(n - ITEM_NADE) as usize] += 1,
            w => {
                if WEAPONS[w as usize].primary() {
                    p.guns.0 = w as u8;
                } else {
                    p.guns.1 = w as u8;
                }
            }
        }
        Ok(())
    }

    /// Dégâts infligés à `victim`. Renvoie les événements (mort, points de vie).
    pub fn hurt(&mut self, attacker: Option<u64>, victim: u64, raw: u32, kind: Hurt) -> Vec<Ev> {
        let mut ev = Vec::new();
        let frozen = matches!(
            self.phase,
            phase::ROUND_END | phase::HALFTIME | phase::GAME_OVER
        );
        let Some(v) = self.players.get(&victim) else {
            return ev;
        };
        if !v.alive || frozen || raw == 0 {
            return ev;
        }
        let vteam = v.team;
        let ateam = attacker.and_then(|a| self.players.get(&a)).map(|a| a.team);
        let friendly = attacker.is_some_and(|a| a != victim) && ateam == Some(vteam);
        let mut dmg = raw as f32;
        if friendly {
            dmg *= match kind {
                Hurt::Grenade => 0.85,
                Hurt::Fire => 0.4,
                _ => 0.33,
            };
        }
        // Gilet (et casque pour la tête) : une partie des dégâts passe, le reste use le
        // gilet (deux points de dégâts par point de gilet).
        let v = self.players.get_mut(&victim).expect("victime");
        let (pen, protected) = match kind {
            Hurt::Bullet { weapon, head, legs } => (
                WEAPONS.get(weapon as usize).map_or(0.5, |w| w.armor_pen()),
                if head { v.helmet } else { !legs },
            ),
            Hurt::Knife => (0.85, true),
            Hurt::Grenade | Hurt::Bomb => (0.5, true),
            Hurt::Fire | Hurt::World => (1.0, false),
        };
        if protected && v.armor > 0 && pen < 1.0 {
            let mut health = dmg * pen;
            let mut armor_loss = (dmg - health) * 0.5;
            if armor_loss > v.armor as f32 {
                armor_loss = v.armor as f32;
                health = dmg - armor_loss * 2.0;
            }
            v.armor = (v.armor as f32 - armor_loss).round().max(0.0) as u8;
            if v.armor == 0 {
                v.helmet = false;
            }
            dmg = health;
        }
        let dmg = dmg.round().max(1.0) as u32;
        let dealt = dmg.min(v.hp);
        v.hp -= dealt;
        let died = v.hp == 0;
        ev.push(Ev::Hp(victim));
        if let Some(a) = attacker.filter(|&a| a != victim && !friendly) {
            if let Some(ap) = self.players.get_mut(&a) {
                ap.damage += dealt;
                ap.round_damage += dealt;
                *ap.dealt.entry(victim).or_default() += dealt;
            }
        }
        if died {
            let head = matches!(kind, Hurt::Bullet { head: true, .. });
            ev.extend(self.kill(victim, attacker.filter(|&a| a != victim), kind, head));
        }
        ev
    }

    fn kill(&mut self, victim: u64, killer: Option<u64>, kind: Hurt, head: bool) -> Vec<Ev> {
        let warmup = !self.started();
        let vteam = {
            let v = self.players.get_mut(&victim).expect("victime");
            v.alive = false;
            v.hp = 0;
            v.dead_for = 0.0;
            v.reset = true;
            v.kit = false;
            v.armor = 0;
            v.helmet = false;
            v.plant_key = false;
            v.defuse_key = false;
            if !warmup {
                v.deaths += 1;
            }
            v.team
        };
        if self.carrier == Some(victim) {
            self.carrier = None;
            self.bomb = bomb::DROPPED;
        }
        if self.defuser == Some(victim) {
            self.defuser = None;
            self.defuse_progress = 0.0;
        }
        if self.planter == Some(victim) && self.bomb == bomb::CARRIED {
            self.plant_progress = 0.0;
        }
        let mut ev = vec![Ev::Killed {
            victim,
            killer,
            weapon: kind.code(),
            head,
        }];
        if warmup {
            return ev;
        }
        if let Some(k) = killer {
            let reward = match kind {
                Hurt::Bullet { weapon, .. } => WEAPONS
                    .get(weapon as usize)
                    .map_or(300, |w| w.kill_reward()),
                Hurt::Knife => 1_500,
                _ => 300,
            };
            if let Some(kp) = self.players.get_mut(&k) {
                if kp.team == vteam {
                    kp.kills = kp.kills.saturating_sub(1);
                    kp.score -= 2;
                    kp.money = kp.money.saturating_sub(TEAMKILL_PENALTY);
                    kp.teamkills += 1;
                    ev.push(Ev::Say(format!(
                        "{} a tué un coéquipier (−300 $, {} / {TEAMKILL_KICK}).",
                        kp.pseudo, kp.teamkills
                    )));
                    if kp.teamkills >= TEAMKILL_KICK {
                        ev.push(Ev::Say(format!(
                            "{} est exclu de la partie ({TEAMKILL_KICK} coéquipiers tués).",
                            kp.pseudo
                        )));
                        ev.push(Ev::TeamKillKick(k));
                    }
                } else {
                    kp.kills += 1;
                    kp.round_kills += 1;
                    kp.score += 2;
                    if head {
                        kp.headshots += 1;
                    }
                    kp.money = (kp.money + reward).min(MAX_MONEY);
                }
            }
        }
        // Passe décisive : au moins 41 points de dégâts à la victime, tuée par un autre.
        let assist = self
            .players
            .iter()
            .filter(|(&id, p)| {
                Some(id) != killer && p.team != vteam && p.dealt.get(&victim).copied() >= Some(41)
            })
            .max_by_key(|(_, p)| p.dealt.get(&victim).copied())
            .map(|(&id, _)| id);
        if let Some(a) = assist {
            if let Some(ap) = self.players.get_mut(&a) {
                ap.assists += 1;
                ap.score += 1;
            }
        }
        ev
    }

    /// Touches de pose / désamorçage d'un joueur.
    pub fn keys(&mut self, id: u64, plant: bool, defuse: bool) {
        if let Some(p) = self.players.get_mut(&id) {
            p.plant_key = plant;
            p.defuse_key = defuse;
        }
    }

    /// Un pas des règles (`places` : où est chaque joueur).
    pub fn tick(&mut self, dt: f32, places: &BTreeMap<u64, Place>) -> Vec<Ev> {
        let mut ev = Vec::new();
        self.ticks = self.ticks.wrapping_add(1);
        self.tick_votes(dt, &mut ev);
        if self.tick_pause(dt, &mut ev) {
            return ev;
        }
        self.time -= dt;
        match self.phase {
            phase::WARMUP => self.tick_warmup(dt, &mut ev),
            phase::FREEZE => {
                if self.time <= 0.0 {
                    self.phase = phase::LIVE;
                    self.time = ROUND_S;
                    self.live_for = 0.0;
                }
            }
            phase::LIVE | phase::PLANTED => {
                self.live_for += dt;
                self.tick_afk(dt, places, &mut ev);
                self.tick_bomb(dt, places, &mut ev);
                self.check_round(&mut ev, places);
            }
            phase::ROUND_END => {
                if self.time <= 0.0 {
                    self.after_round(&mut ev);
                }
            }
            phase::HALFTIME if self.time <= 0.0 => self.new_round(&mut ev),
            _ => {}
        }
        ev
    }

    fn tick_warmup(&mut self, dt: f32, ev: &mut Vec<Ev>) {
        // Réapparitions rapides.
        let dead: Vec<u64> = self
            .players
            .iter_mut()
            .filter(|(_, p)| p.present && !p.alive)
            .filter_map(|(&id, p)| {
                p.dead_for += dt;
                (p.dead_for >= WARMUP_RESPAWN_S).then_some(id)
            })
            .collect();
        for id in dead {
            if let Some(p) = self.players.get_mut(&id) {
                p.alive = true;
                p.hp = 100;
                p.money = MAX_MONEY;
            }
            ev.push(self.spawn(id, true));
        }
        let full = self.count(CT) >= self.team_max() && self.count(T) >= self.team_max();
        if !full {
            if self.countdown {
                ev.push(Ev::Say(
                    "Il manque des joueurs : retour à l'échauffement.".into(),
                ));
            }
            self.countdown = false;
            self.time = 0.0;
            return;
        }
        if !self.countdown {
            self.countdown = true;
            self.time = START_S;
            ev.push(Ev::Say(format!(
                "{} joueurs : la partie commence dans {} s.",
                self.team_max() * 2,
                START_S as u32
            )));
            return;
        }
        if self.time <= 0.0 {
            self.countdown = false;
            self.start(ev);
        }
    }

    /// Début de la partie : statistiques à zéro, 800 $, manche 1.
    fn start(&mut self, ev: &mut Vec<Ev>) {
        for p in self.players.values_mut() {
            p.start_team = p.team;
            p.money = START_MONEY;
            p.kills = 0;
            p.deaths = 0;
            p.assists = 0;
            p.headshots = 0;
            p.damage = 0;
            p.mvps = 0;
            p.score = 0;
            p.rounds = 0;
            p.armor = 0;
            p.helmet = false;
            p.kit = false;
            p.reset = true;
        }
        self.players.retain(|_, p| p.present);
        self.swapped = false;
        self.played = 0;
        self.wins = [0, 0];
        self.loss_streak = [0, 0];
        self.history.clear();
        ev.push(Ev::Started);
        ev.push(Ev::Say(
            "La partie commence : 24 manches, premier à 13.".into(),
        ));
        self.new_round(ev);
    }

    fn new_round(&mut self, ev: &mut Vec<Ev>) {
        self.phase = phase::FREEZE;
        self.time = FREEZE_S;
        self.live_for = 0.0;
        self.bomb = bomb::NONE;
        self.carrier = None;
        self.bomb_lock = None;
        self.planter = None;
        self.plant_progress = 0.0;
        self.defuser = None;
        self.defuse_progress = 0.0;
        self.planted_this_round = false;
        let ids: Vec<u64> = self
            .players
            .iter()
            .filter(|(_, p)| p.present)
            .map(|(&id, _)| id)
            .collect();
        for id in &ids {
            let p = self.players.get_mut(id).expect("joueur");
            let reset = p.reset || !p.alive;
            p.alive = true;
            p.hp = 100;
            p.round_kills = 0;
            p.round_damage = 0;
            p.dealt.clear();
            p.dead_for = 0.0;
            p.reset = false;
            ev.push(self.spawn(*id, reset));
        }
        // La bombe à un T au hasard.
        let ts: Vec<u64> = ids
            .iter()
            .copied()
            .filter(|id| self.players[id].team == T)
            .collect();
        if !ts.is_empty() {
            let i = self.rand() as usize % ts.len();
            self.carrier = Some(ts[i]);
            self.bomb = bomb::CARRIED;
        }
        self.ground.clear();
        ev.push(Ev::NewRound);
        ev.push(Ev::Say(format!("Manche {}.", self.played + 1)));
        self.compensate(ev);
    }

    /// Départ d'une manche : une équipe à qui il manque un joueur depuis au moins
    /// `ABSENT_ROUNDS` manches (déconnecté, sorti pour inactivité, exclu pour avoir tué
    /// ses coéquipiers — pas exclu par un vote de l'équipe, qui en profiterait) reçoit
    /// `ABSENT_COMP` par absent, partagé entre les présents.
    fn compensate(&mut self, ev: &mut Vec<Ev>) {
        self.comp.clear();
        for team in [CT, T] {
            let absent: Vec<u64> = self
                .players
                .iter()
                .filter(|(_, p)| {
                    !p.present && p.team == team && !p.kicked && p.missed >= ABSENT_ROUNDS
                })
                .map(|(&id, _)| id)
                .collect();
            let here = self.count_present(team) as u32;
            if absent.is_empty() || here == 0 {
                continue;
            }
            // Part de chacun pour un absent, arrondie à 50 $.
            let part = (ABSENT_COMP / here).div_ceil(50) * 50;
            let each = part * absent.len() as u32;
            for p in self.players.values_mut() {
                if p.present && p.team == team {
                    p.money = (p.money + each).min(MAX_MONEY);
                }
            }
            for &a in &absent {
                self.comp.push((a, team, part));
            }
            let side = if team == CT {
                "antiterroristes"
            } else {
                "terroristes"
            };
            let who = if absent.len() == 1 {
                "1 joueur absent".to_string()
            } else {
                format!("{} joueurs absents", absent.len())
            };
            ev.push(Ev::Say(format!(
                "Compensation ({side}, {who}) : +${each} par joueur présent."
            )));
        }
        for p in self.players.values_mut() {
            if !p.present {
                p.missed = p.missed.saturating_add(1);
            }
        }
    }

    fn count_present(&self, team: u8) -> usize {
        self.players
            .values()
            .filter(|p| p.present && p.team == team)
            .count()
    }

    /// Les entrées d'un pas de `id` (leur empreinte) : si elles ont changé, il joue.
    pub fn input_seen(&mut self, id: u64, sig: u64) {
        if let Some(p) = self.players.get_mut(&id) {
            if p.last_input != sig {
                p.last_input = sig;
                p.idle = 0.0;
            }
        }
    }

    /// `id` vient de faire quelque chose (tir, achat, vote, message) : il joue.
    pub fn touch(&mut self, id: u64) {
        if let Some(p) = self.players.get_mut(&id) {
            p.idle = 0.0;
        }
    }

    /// Inactivité : le temps passé en vie dans une manche en cours sans rien toucher
    /// s'ajoute d'une manche à l'autre ; à `AFK_S`, le joueur sort de la partie comme s'il
    /// s'était déconnecté (il peut revenir).
    fn tick_afk(&mut self, dt: f32, places: &BTreeMap<u64, Place>, ev: &mut Vec<Ev>) {
        let mut out = Vec::new();
        for (&id, p) in self.players.iter_mut() {
            if p.present && p.alive {
                p.idle += dt;
                if p.idle >= AFK_S {
                    out.push(id);
                }
            }
        }
        for id in out {
            let pending = self.tech_pending;
            let name = self.players[&id].pseudo.clone();
            self.leave(id, places.get(&id).map(|pl| pl.feet));
            // Pas de pause technique pour un joueur inactif.
            self.tech_pending = pending;
            if let Some(p) = self.players.get_mut(&id) {
                p.idle = 0.0;
            }
            ev.push(Ev::Say(format!(
                "{name} est sorti de la partie pour inactivité (il peut revenir)."
            )));
            ev.push(Ev::AfkKick(id));
        }
    }

    fn tick_bomb(&mut self, dt: f32, places: &BTreeMap<u64, Place>, ev: &mut Vec<Ev>) {
        match self.bomb {
            bomb::CARRIED => {
                let Some(c) = self.carrier else { return };
                if let Some(pl) = places.get(&c) {
                    self.bomb_pos = pl.feet;
                }
                let holding = self.players.get(&c).is_some_and(|p| p.plant_key && p.alive);
                let site = places.get(&c).and_then(|pl| {
                    self.map
                        .sites
                        .iter()
                        .position(|s| s.is_some_and(|z| z.contains(pl.feet.to_array())))
                        .filter(|_| pl.on_ground && pl.speed < 0.6)
                });
                // Sans site défini, on pose n'importe où.
                let no_sites = self.map.sites.iter().all(|s| s.is_none());
                let site = site.or_else(|| {
                    no_sites
                        .then(|| places.get(&c).filter(|pl| pl.on_ground && pl.speed < 0.6))
                        .flatten()
                        .map(|_| 0)
                });
                match (holding && self.phase == phase::LIVE, site) {
                    (true, Some(s)) => {
                        self.planter = Some(c);
                        self.plant_progress += dt;
                        if self.plant_progress >= PLANT_S {
                            self.bomb = bomb::PLANTED;
                            self.carrier = None;
                            self.site = s as u8;
                            self.phase = phase::PLANTED;
                            self.time = BOMB_S;
                            self.planted_this_round = true;
                            if let Some(p) = self.players.get_mut(&c) {
                                p.money = (p.money + OBJECTIVE_REWARD).min(MAX_MONEY);
                                p.score += 2;
                                p.plant_key = false;
                            }
                            ev.push(Ev::Say(format!(
                                "La bombe a été posée sur le site {}.",
                                crate::fpsmap::SITE_NAMES[s]
                            )));
                        }
                    }
                    _ => self.plant_progress = 0.0,
                }
            }
            bomb::DROPPED => {
                // Un T vivant qui passe dessus la ramasse (sauf celui qui vient de la lâcher).
                if let Some((_, t)) = &mut self.bomb_lock {
                    *t -= dt;
                    if *t <= 0.0 {
                        self.bomb_lock = None;
                    }
                }
                let locked = self.bomb_lock.map(|l| l.0);
                let taker = places
                    .iter()
                    .filter(|(id, _)| {
                        Some(**id) != locked
                            && self
                                .players
                                .get(id)
                                .is_some_and(|p| p.alive && p.present && p.team == T)
                    })
                    .find(|(_, pl)| (pl.feet - self.bomb_pos).length() < PICKUP)
                    .map(|(&id, _)| id);
                if let Some(id) = taker {
                    self.carrier = Some(id);
                    self.bomb = bomb::CARRIED;
                }
            }
            bomb::PLANTED => {
                // Désamorçage : un CT vivant, au sol, près de la bombe, touche maintenue.
                let bomb_pos = self.bomb_pos;
                let ok = |id: u64, s: &Self| {
                    s.players
                        .get(&id)
                        .is_some_and(|p| p.alive && p.defuse_key && p.team == CT)
                        && places.get(&id).is_some_and(|pl| {
                            pl.on_ground && (pl.feet - bomb_pos).length() < DEFUSE_REACH
                        })
                };
                if let Some(d) = self.defuser {
                    if !ok(d, self) {
                        self.defuser = None;
                        self.defuse_progress = 0.0;
                    }
                }
                if self.defuser.is_none() {
                    let ids: Vec<u64> = places.keys().copied().collect();
                    self.defuser = ids.into_iter().find(|&id| ok(id, self));
                }
                if let Some(d) = self.defuser {
                    self.defuse_progress += dt;
                    let need = if self.players[&d].kit {
                        DEFUSE_KIT_S
                    } else {
                        DEFUSE_S
                    };
                    if self.defuse_progress >= need && self.time > 0.0 {
                        self.bomb = bomb::DEFUSED;
                        if let Some(p) = self.players.get_mut(&d) {
                            p.money = (p.money + OBJECTIVE_REWARD).min(MAX_MONEY);
                            p.score += 2;
                        }
                        ev.push(Ev::Say("Bombe désamorcée.".into()));
                    }
                }
                if self.time <= 0.0 && self.bomb == bomb::PLANTED {
                    self.bomb = bomb::EXPLODED;
                    ev.push(Ev::Say("La bombe a explosé.".into()));
                    // Dégâts à tous ceux qui sont proches.
                    let hits: Vec<(u64, u32)> = places
                        .iter()
                        .map(|(&id, pl)| {
                            let d = (pl.feet + Vec3::Y - bomb_pos).length();
                            let dmg =
                                BOMB_DAMAGE * (-(d * d) / (2.0 * BOMB_SIGMA * BOMB_SIGMA)).exp();
                            (id, dmg as u32)
                        })
                        .filter(|(_, d)| *d > 0)
                        .collect();
                    for (id, d) in hits {
                        ev.extend(self.hurt(None, id, d, Hurt::Bomb));
                    }
                }
            }
            _ => {}
        }
    }

    fn check_round(&mut self, ev: &mut Vec<Ev>, _places: &BTreeMap<u64, Place>) {
        let (ct, t) = (self.alive(CT), self.alive(T));
        let end = match self.bomb {
            bomb::EXPLODED => Some((T, reason::EXPLODED)),
            bomb::DEFUSED => Some((CT, reason::DEFUSED)),
            bomb::PLANTED => (ct == 0).then_some((T, reason::CT_ELIMINATED)),
            _ => {
                if t == 0 {
                    Some((CT, reason::T_ELIMINATED))
                } else if ct == 0 {
                    Some((T, reason::CT_ELIMINATED))
                } else if self.time <= 0.0 {
                    Some((CT, reason::TIME))
                } else {
                    None
                }
            }
        };
        if let Some((winner, why)) = end {
            self.end_round(winner, why, ev);
        }
    }

    fn end_round(&mut self, winner: u8, why: u8, ev: &mut Vec<Ev>) {
        self.phase = phase::ROUND_END;
        self.time = ROUND_END_S;
        self.played += 1;
        let start = self.start_of(winner);
        self.wins[start as usize] += 1;
        self.history.push((start, why));
        let loser = other(winner);
        self.loss_streak[winner as usize] = self.loss_streak[winner as usize].saturating_sub(1);
        self.loss_streak[loser as usize] = (self.loss_streak[loser as usize] + 1).min(5);
        let win_money = if matches!(why, reason::EXPLODED | reason::DEFUSED) {
            WIN_BOMB
        } else {
            WIN_ELIM
        };
        let loss_money =
            LOSS_BASE + LOSS_STEP * (self.loss_streak[loser as usize].saturating_sub(1)).min(4);
        let planted = self.planted_this_round;
        for p in self.players.values_mut().filter(|p| p.present) {
            p.rounds = p.rounds.saturating_add(1);
            let gain = if p.team == winner {
                win_money
            } else if why == reason::TIME && p.team == T && p.alive {
                // T qui laissent passer le temps sans poser : rien.
                0
            } else {
                loss_money
                    + if p.team == T && planted {
                        PLANT_TEAM_BONUS
                    } else {
                        0
                    }
            };
            p.money = (p.money + gain).min(MAX_MONEY);
            // Les survivants gardent leur équipement.
            p.reset = !p.alive;
        }
        // Homme du match.
        let mvp = match why {
            reason::DEFUSED => self.defuser,
            reason::EXPLODED => self.planter,
            _ => self
                .players
                .iter()
                .filter(|(_, p)| p.present && p.team == winner)
                .max_by_key(|(_, p)| (p.round_kills, p.round_damage))
                .map(|(&id, _)| id),
        };
        if let Some(p) = mvp.and_then(|id| self.players.get_mut(&id)) {
            p.mvps += 1;
        }
        let side = if winner == CT { "Les CT" } else { "Les T" };
        let how = match why {
            reason::T_ELIMINATED | reason::CT_ELIMINATED => "par élimination",
            reason::TIME => "au temps",
            reason::EXPLODED => "par l'explosion",
            _ => "par le désamorçage",
        };
        let s = self.score_sides();
        ev.push(Ev::RoundEnd {
            winner,
            reason: why,
        });
        ev.push(Ev::Say(format!(
            "{side} gagnent la manche {how} - CT {} : {} T.",
            s[0], s[1]
        )));
    }

    /// Partie finie ?
    fn finished(&self) -> bool {
        let (a, b) = (self.wins[0], self.wins[1]);
        let n = a as u32 + b as u32;
        if n <= 2 * HALF {
            return a >= WIN || b >= WIN;
        }
        let block = (n - 2 * HALF - 1) / 6;
        let target = (HALF + 3 * block) as u8 + 4;
        a >= target || b >= target
    }

    fn after_round(&mut self, ev: &mut Vec<Ev>) {
        if self.finished() {
            self.phase = phase::GAME_OVER;
            self.time = 0.0;
            let summary = self.summary();
            let (a, b) = (summary.score[0], summary.score[1]);
            ev.push(Ev::Say(format!("Fin de la partie : {a} - {b}.")));
            ev.push(Ev::GameOver(summary));
            return;
        }
        let n = self.played;
        let swap = n == HALF || (n >= 2 * HALF && (n - 2 * HALF).is_multiple_of(3));
        if swap {
            self.swapped = !self.swapped;
            for p in self.players.values_mut() {
                p.team = other(p.team);
                p.reset = true;
                p.armor = 0;
                p.helmet = false;
                p.kit = false;
                p.money = if n >= 2 * HALF { OT_MONEY } else { START_MONEY };
            }
            self.loss_streak = [0, 0];
            self.phase = phase::HALFTIME;
            self.time = HALFTIME_S;
            let msg = if n == 2 * HALF {
                "12 partout : prolongations (6 manches, premier à 4). Changement de côté."
            } else if n > HALF {
                "Mi-temps des prolongations : changement de côté."
            } else {
                "Mi-temps : changement de côté."
            };
            ev.push(Ev::Say(msg.into()));
            return;
        }
        self.new_round(ev);
    }

    pub fn summary(&self) -> MatchSummary {
        let mut players: Vec<SummaryPlayer> = self
            .players
            .values()
            .map(|p| SummaryPlayer {
                pseudo: p.pseudo.clone(),
                start_team: p.start_team,
                kills: p.kills,
                deaths: p.deaths,
                assists: p.assists,
                headshots: p.headshots,
                damage: p.damage,
                mvps: p.mvps,
                score: p.score,
                rounds: p.rounds,
                teamkills: p.teamkills,
                kicked: p.kicked,
            })
            .collect();
        players.sort_by(|a, b| a.pseudo.cmp(&b.pseudo));
        // Abandon : l'autre équipe gagne (au moins 13 manches, et plus que la perdante).
        let mut score = self.wins;
        if let Some(loser) = self.surrendered {
            let (l, w) = (loser as usize & 1, 1 - (loser as usize & 1));
            score[w] = score[w].max(WIN).max(score[l].saturating_add(1));
        }
        MatchSummary {
            map_sig: self.map_sig,
            score,
            rounds: self.played.min(255) as u8,
            players,
        }
    }

    /// HUD d'un joueur.
    pub fn hud(&self, id: u64, at: Option<Vec3>) -> MatchHud {
        let p = self.players.get(&id);
        let progress = if self.planter == Some(id) && self.bomb == bomb::CARRIED {
            self.plant_progress / PLANT_S
        } else if self.defuser == Some(id) {
            let need = if p.is_some_and(|p| p.kit) {
                DEFUSE_KIT_S
            } else {
                DEFUSE_S
            };
            self.defuse_progress / need
        } else {
            0.0
        };
        // La bombe n'est montrée qu'aux T, sauf posée.
        let team = p.map_or(CT, |p| p.team);
        let bomb_state = match self.bomb {
            bomb::CARRIED if self.carrier == Some(id) => bomb::CARRIED,
            bomb::CARRIED | bomb::DROPPED if team != T => bomb::NONE,
            b => b,
        };
        MatchHud {
            phase: self.phase,
            time: self.time.max(0.0),
            round: if self.started() {
                (self.played + 1).min(255) as u8
            } else {
                0
            },
            score: self.score_sides(),
            team,
            money: p.map_or(0, |p| p.money),
            armor: p.map_or(0, |p| p.armor),
            helmet: p.is_some_and(|p| p.helmet),
            kit: p.is_some_and(|p| p.kit),
            bomb: bomb_state,
            carrying: self.bomb == bomb::CARRIED && self.carrier == Some(id),
            bomb_pos: self.bomb_pos.to_array(),
            progress: progress.clamp(0.0, 1.0),
            can_buy: self.can_buy(id, at),
            alive: [self.alive(CT) as u8, self.alive(T) as u8],
            site: self.site,
            players: self.present() as u8,
            pause: self.pause.map_or(0, |p| p.kind),
            pause_left: self.pause.map_or(0.0, |p| p.left.max(0.0)),
            timeouts: TIMEOUTS.saturating_sub(self.timeouts[self.start_side(team)]),
            vote: self
                .votes
                .iter()
                .find(|v| v.team == team && p.is_some())
                .map(|v| VoteHud {
                    kind: v.kind,
                    caller: v.caller,
                    target: v.target,
                    yes: v.yes.len() as u8,
                    no: v.no.len() as u8,
                    need: self.needed(v) as u8,
                    left: v.left.max(0.0),
                    can_vote: self.voters(v).contains(&id)
                        && !v.yes.contains(&id)
                        && !v.no.contains(&id),
                }),
        }
    }

    pub fn rows(&self) -> Vec<ScoreRow> {
        let mut v: Vec<ScoreRow> = self
            .players
            .iter()
            .filter(|(_, p)| p.present || self.started())
            .map(|(&id, p)| ScoreRow {
                id,
                pseudo: self.shown(id),
                team: p.team,
                kills: p.kills,
                deaths: p.deaths,
                assists: p.assists,
                mvps: p.mvps,
                headshots: p.headshots,
                damage: p.damage,
                score: p.score,
                alive: p.alive && p.present,
                money: p.money,
                elo: 0,
                color: p.color,
            })
            .collect();
        v.sort_by(|a, b| b.score.cmp(&a.score).then(b.kills.cmp(&a.kills)));
        v
    }

    pub fn flags(&self, id: u64) -> u8 {
        use minelib_net::game::{FLAG_BOMB, FLAG_DEFUSING, FLAG_KIT, FLAG_PLANTING};
        let mut f = 0;
        if self.carrier == Some(id) {
            f |= FLAG_BOMB;
        }
        if self.players.get(&id).is_some_and(|p| p.kit) {
            f |= FLAG_KIT;
        }
        if self.defuser == Some(id) {
            f |= FLAG_DEFUSING;
        }
        if self.planter == Some(id) && self.plant_progress > 0.0 {
            f |= FLAG_PLANTING;
        }
        f
    }

    /// Immobile (départ de manche, mi-temps, fin).
    /// Équipe de départ (indice de `timeouts`) des joueurs de l'équipe `team` (côté
    /// actuel).
    fn start_side(&self, team: u8) -> usize {
        ((team == T) != self.swapped) as usize
    }

    /// Joueurs qui votent : coéquipiers présents, sauf le joueur visé.
    fn voters(&self, v: &Vote) -> Vec<u64> {
        self.players
            .iter()
            .filter(|(&id, p)| {
                p.present && p.team == v.team && !(v.kind == VOTE_KICK && id == v.target)
            })
            .map(|(&id, _)| id)
            .collect()
    }

    /// Oui nécessaires : tous les votants (exclusion, abandon), la majorité (pause).
    fn needed(&self, v: &Vote) -> usize {
        let n = self.voters(v).len();
        if v.kind == VOTE_KICK || v.kind == VOTE_SURRENDER {
            n
        } else {
            n / 2 + 1
        }
    }

    /// `id` propose un vote à son équipe (`VOTE_KICK` : exclure `target`).
    pub fn call_vote(&mut self, id: u64, kind: u8, target: u64) -> Result<Vec<Ev>, String> {
        let Some(me) = self.players.get(&id).filter(|p| p.present) else {
            return Err("Tu n'es pas dans la partie.".into());
        };
        let team = me.team;
        if self.phase == phase::GAME_OVER {
            return Err("La partie est finie.".into());
        }
        if self.votes.iter().any(|v| v.team == team) {
            return Err("Un vote est déjà en cours dans ton équipe.".into());
        }
        let what = match kind {
            VOTE_KICK => {
                let Some(t) = self
                    .players
                    .get(&target)
                    .filter(|t| t.present && t.team == team && target != id)
                else {
                    return Err("Ce joueur n'est pas un coéquipier.".into());
                };
                format!("exclure {}", t.pseudo)
            }
            VOTE_PAUSE => {
                if !self.started() {
                    return Err("Pas de pause pendant l'échauffement.".into());
                }
                let side = self.start_side(team);
                let pending = self
                    .pending_timeouts
                    .iter()
                    .filter(|&&s| s as usize == side);
                if self.timeouts[side] as usize + pending.count() >= TIMEOUTS as usize {
                    return Err(format!("Ton équipe a déjà pris ses {TIMEOUTS} pauses."));
                }
                "une pause tactique".to_string()
            }
            VOTE_SURRENDER => {
                if !self.started() {
                    return Err("Pas d'abandon pendant l'échauffement.".into());
                }
                "d'abandonner la partie".to_string()
            }
            _ => return Err("Vote inconnu.".into()),
        };
        let v = Vote {
            kind,
            team,
            caller: id,
            target: if kind == VOTE_KICK { target } else { u64::MAX },
            yes: vec![id],
            no: Vec::new(),
            left: VOTE_S,
        };
        let caller = me.pseudo.clone();
        self.votes.push(v);
        let mut ev = vec![Ev::Say(format!(
            "{caller} propose {what} (F1 oui, F2 non)."
        ))];
        self.settle_votes(&mut ev);
        Ok(ev)
    }

    /// Réponse de `id` au vote de son équipe.
    pub fn ballot(&mut self, id: u64, yes: bool) -> Vec<Ev> {
        let mut ev = Vec::new();
        let Some(team) = self.players.get(&id).filter(|p| p.present).map(|p| p.team) else {
            return ev;
        };
        let Some(k) = self.votes.iter().position(|v| v.team == team) else {
            return ev;
        };
        let voters = self.voters(&self.votes[k]);
        let v = &mut self.votes[k];
        if !voters.contains(&id) || v.yes.contains(&id) || v.no.contains(&id) {
            return ev;
        }
        if yes {
            v.yes.push(id);
        } else {
            v.no.push(id);
        }
        self.settle_votes(&mut ev);
        ev
    }

    fn tick_votes(&mut self, dt: f32, ev: &mut Vec<Ev>) {
        for v in &mut self.votes {
            v.left -= dt;
        }
        self.settle_votes(ev);
    }

    /// Votes décidés : acceptés (assez de oui), refusés (trop de non, temps écoulé).
    fn settle_votes(&mut self, ev: &mut Vec<Ev>) {
        let mut k = 0;
        while k < self.votes.len() {
            let v = self.votes[k].clone();
            let voters = self.voters(&v).len();
            let need = self.needed(&v);
            let yes = v.yes.len();
            let passed = yes >= need && voters > 0;
            let failed = !passed && (v.left <= 0.0 || voters.saturating_sub(v.no.len()) < need);
            if !passed && !failed {
                k += 1;
                continue;
            }
            self.votes.remove(k);
            if failed {
                ev.push(Ev::Say("Vote refusé.".into()));
                continue;
            }
            match v.kind {
                VOTE_KICK => {
                    if let Some(t) = self.players.get_mut(&v.target) {
                        t.kicked = true;
                        ev.push(Ev::Say(format!("{} est exclu par son équipe.", t.pseudo)));
                    }
                    ev.push(Ev::VoteKick(v.target));
                }
                VOTE_SURRENDER => {
                    // L'équipe abandonne : la partie s'arrête, gagnée par l'autre.
                    let loser = self.start_of(v.team);
                    self.surrendered = Some(loser);
                    self.votes.clear();
                    self.pause = None;
                    self.phase = phase::GAME_OVER;
                    self.time = 0.0;
                    let side = if v.team == CT {
                        "antiterroristes"
                    } else {
                        "terroristes"
                    };
                    ev.push(Ev::Say(format!("Les {side} abandonnent la partie.")));
                    let summary = self.summary();
                    let (a, b) = (summary.score[0], summary.score[1]);
                    ev.push(Ev::Say(format!("Fin de la partie : {a} - {b}.")));
                    ev.push(Ev::GameOver(summary));
                    return;
                }
                _ => {
                    let side = self.start_side(v.team) as u8;
                    self.pending_timeouts.push(side);
                    ev.push(Ev::Say(
                        "Pause tactique acceptée : au départ de la prochaine manche.".into(),
                    ));
                }
            }
        }
    }

    /// Pauses : prises au départ d'une manche, le temps de départ ne s'écoule pas.
    /// Rend vrai si le jeu est en pause.
    fn tick_pause(&mut self, dt: f32, ev: &mut Vec<Ev>) -> bool {
        if self.phase != phase::FREEZE {
            return false;
        }
        if self.pause.is_none() {
            if self.tech_pending && self.absent() > 0 {
                self.tech_pending = false;
                self.pause = Some(Pause {
                    kind: PAUSE_TECHNICAL,
                    left: TECH_PAUSE_S,
                });
                ev.push(Ev::Say(
                    "Pause technique : un joueur s'est déconnecté (5 minutes au plus).".into(),
                ));
                ev.push(Ev::Paused(true));
            } else if !self.pending_timeouts.is_empty() {
                let side = self.pending_timeouts.remove(0) as usize;
                self.timeouts[side] = (self.timeouts[side] + 1).min(TIMEOUTS);
                self.pause = Some(Pause {
                    kind: PAUSE_TACTICAL,
                    left: TIMEOUT_S,
                });
                ev.push(Ev::Say(format!(
                    "Pause tactique ({} / {TIMEOUTS}).",
                    self.timeouts[side]
                )));
                ev.push(Ev::Paused(true));
            } else {
                self.tech_pending = false;
                return false;
            }
        }
        let absent = self.absent();
        let p = self.pause.as_mut().expect("pause");
        p.left -= dt;
        let back = p.kind == PAUSE_TECHNICAL && absent == 0;
        if p.left <= 0.0 || back {
            self.pause = None;
            ev.push(Ev::Say("Fin de la pause.".into()));
            ev.push(Ev::Paused(false));
            // Une autre pause votée attend peut-être : au prochain pas.
            return true;
        }
        true
    }

    /// Joueurs partis (ni exclus) qui peuvent revenir.
    fn absent(&self) -> usize {
        self.players
            .values()
            .filter(|p| !p.present && !p.excluded())
            .count()
    }

    pub fn frozen(&self) -> bool {
        matches!(
            self.phase,
            phase::FREEZE | phase::HALFTIME | phase::GAME_OVER
        )
    }

    /// En train de poser ou de désamorcer : immobile aussi (comme CS).
    pub fn busy(&self, id: u64) -> bool {
        (self.planter == Some(id) && self.plant_progress > 0.0 && self.bomb == bomb::CARRIED)
            || self.defuser == Some(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minelib_net::game::FpsMap;

    #[test]
    fn a_lobby_plays_on_the_same_team() {
        for seed in 1..20 {
            let mut m = Match::new(FpsMap::default(), [0; 32], seed);
            m.join(0, "Seul");
            for i in 2..7u64 {
                m.join_party(i, &format!("Ami{i}"), 42);
            }
            let team = m.players[&2].team;
            assert!((2..7).all(|i| m.players[&i].team == team), "graine {seed}");
            // L'équipe du lobby est pleine : les suivants vont en face.
            m.join(7, "Tard");
            assert_ne!(m.players[&7].team, team);
        }
    }

    #[test]
    fn a_team_uses_only_the_first_colors_of_its_size() {
        // 2 contre 2 : bleu et orange seulement, même si l'on préfère le violet.
        let mut map = FpsMap::default();
        map.set_players(4);
        let mut m = Match::new(map, [0; 32], 3);
        m.join_party(1, "A", 9);
        m.join_party(2, "B", 9);
        m.set_color(1, 4);
        m.set_color(2, 1);
        assert_eq!(m.players[&2].color, 1);
        assert_eq!(m.players[&1].color, 0);
        // 5 contre 5 : le violet (5e couleur) est permis.
        let mut m = Match::new(FpsMap::default(), [0; 32], 3);
        m.join(1, "A");
        m.set_color(1, 4);
        assert_eq!(m.players[&1].color, 4);
    }

    #[test]
    fn team_votes_kick_and_pause_like_cs() {
        let mut m = full_match();
        let places = BTreeMap::new();
        let team_of = |m: &Match, t: u8| {
            m.players
                .iter()
                .filter(|(_, p)| p.team == t)
                .map(|(&i, _)| i)
                .collect::<Vec<u64>>()
        };
        let ct = team_of(&m, CT);
        // Pause tactique : la majorité (3 sur 5).
        m.call_vote(ct[0], VOTE_PAUSE, 0).unwrap();
        assert!(
            m.call_vote(ct[1], VOTE_PAUSE, 0).is_err(),
            "un vote à la fois"
        );
        m.ballot(ct[1], true);
        assert_eq!(m.votes.len(), 1);
        m.ballot(ct[2], true);
        assert!(m.votes.is_empty());
        // Au départ de la manche : le temps de départ ne s'écoule plus pendant 30 s.
        let before = m.time;
        assert!(m.tick(1.0, &places).contains(&Ev::Paused(true)));
        assert_eq!(m.time, before);
        assert_eq!(m.hud(ct[0], None).pause, PAUSE_TACTICAL);
        for _ in 0..30 {
            m.tick(1.0, &places);
        }
        assert!(m.pause.is_none());
        assert_eq!(m.hud(ct[0], None).timeouts, TIMEOUTS - 1);
        // Exclusion : tous les autres coéquipiers d'accord ; le visé ne vote pas.
        m.call_vote(ct[0], VOTE_KICK, ct[4]).unwrap();
        assert!(m.hud(ct[4], None).vote.is_some_and(|v| !v.can_vote));
        assert!(m
            .hud(ct[1], None)
            .vote
            .is_some_and(|v| v.can_vote && v.need == 4));
        m.ballot(ct[4], false);
        m.ballot(ct[1], true);
        m.ballot(ct[2], true);
        assert!(!m.players[&ct[4]].kicked);
        assert!(m.ballot(ct[3], true).contains(&Ev::VoteKick(ct[4])));
        let name = m.players[&ct[4]].pseudo.clone();
        m.leave(ct[4], None);
        assert!(!m.tech_pending, "exclu : pas de pause technique");
        assert_eq!(m.join(99, &name), vec![Ev::VoteKick(99)]);
        assert!(m.summary().players.iter().any(|p| p.kicked));
        // Un T se déconnecte : pause technique jusqu'à son retour.
        let t = team_of(&m, T)[0];
        let name = m.players[&t].pseudo.clone();
        m.leave(t, None);
        assert!(m.tick(1.0, &places).contains(&Ev::Paused(true)));
        assert_eq!(m.pause.map(|p| p.kind), Some(PAUSE_TECHNICAL));
        m.join(500, &name);
        assert!(m.tick(1.0, &places).contains(&Ev::Paused(false)));
        // Un refus suffit à rejeter une exclusion.
        m.call_vote(ct[0], VOTE_KICK, ct[1]).unwrap();
        m.ballot(ct[2], false);
        assert!(m.votes.is_empty());
    }

    /// Avance de `secs` pendant que tous les joueurs jouent (bougent).
    fn play(m: &mut Match, secs: f32) -> Vec<Ev> {
        let ids: Vec<u64> = m.players.keys().copied().collect();
        let mut ev = Vec::new();
        for s in 0..secs.ceil() as u64 {
            for &id in &ids {
                m.input_seen(id, 1 + s % 2);
            }
            ev.extend(run(m, 1.0));
        }
        ev
    }

    fn says(ev: &[Ev], part: &str) -> bool {
        ev.iter()
            .any(|e| matches!(e, Ev::Say(t) if t.contains(part)))
    }

    #[test]
    fn an_idle_living_player_leaves_after_two_minutes_and_may_come_back() {
        let mut m = full_match();
        run(&mut m, FREEZE_S + 0.1);
        assert_eq!(m.phase, phase::LIVE);
        let afk = *m.players.iter().find(|(_, p)| p.team == CT).unwrap().0;
        let name = m.players[&afk].pseudo.clone();
        // Tous jouent sauf lui, pendant 100 s de manche : rien encore.
        let ids: Vec<u64> = m.players.keys().copied().filter(|&i| i != afk).collect();
        let mut ev = Vec::new();
        let mut live = 0.0;
        let step = |m: &mut Match, ev: &mut Vec<Ev>| {
            for &id in &ids {
                m.touch(id);
            }
            let before = m.phase;
            ev.extend(run(m, 1.0));
            matches!(before, phase::LIVE | phase::PLANTED) as u32 as f32
        };
        while live < 100.0 {
            live += step(&mut m, &mut ev);
        }
        assert!(!ev.iter().any(|e| matches!(e, Ev::AfkKick(_))));
        assert!(m.players[&afk].present);
        // Le temps s'ajoute d'une manche à l'autre (pas pendant le départ, ni mort) :
        // à 2 minutes en vie sans rien toucher, il sort.
        while live < 125.0 && !ev.contains(&Ev::AfkKick(afk)) {
            live += step(&mut m, &mut ev);
        }
        assert!(ev.contains(&Ev::AfkKick(afk)), "sorti après {live} s");
        assert!(live >= 115.0, "pas avant deux minutes : {live} s");
        assert!(says(&ev, "inactivité"));
        let p = &m.players[&afk];
        assert!(!p.present && !p.alive && !p.excluded());
        // Pas de pause technique pour ça.
        assert!(!m.tech_pending);
        // Personne d'autre n'est sorti.
        assert_eq!(ev.iter().filter(|e| matches!(e, Ev::AfkKick(_))).count(), 1);
        // Il revient (même pseudo, nouvelle connexion) : il retrouve sa place.
        let back = m.join(500, &name);
        assert!(says(&back, "de retour"));
        assert!(m.players[&500].present && m.players[&500].idle == 0.0);
        // Un joueur qui bouge la souris ou tire n'est jamais sorti.
        let mut m = full_match();
        let ev = play(&mut m, FREEZE_S + 300.0);
        assert!(!ev.iter().any(|e| matches!(e, Ev::AfkKick(_))));
    }

    #[test]
    fn a_team_short_of_players_for_two_rounds_is_compensated_until_they_return() {
        let mut m = full_match();
        let cts: Vec<u64> = m
            .players
            .iter()
            .filter(|(_, p)| p.team == CT)
            .map(|(&i, _)| i)
            .collect();
        let (gone, gone2) = (cts[0], cts[1]);
        let names = [
            m.players[&gone].pseudo.clone(),
            m.players[&gone2].pseudo.clone(),
        ];
        let mate = cts[4];
        // Fin de manche rapide : les T sont tués, puis départ de la suivante.
        let next_round = |m: &mut Match| {
            let mut ev = play(m, FREEZE_S + 0.5);
            kill_team(m, T);
            let n = m.played;
            while m.played == n || m.phase != phase::FREEZE {
                ev.extend(play(m, 1.0));
            }
            ev
        };
        play(&mut m, FREEZE_S + 1.0);
        m.leave(gone, None);
        kill_team(&mut m, T);
        // Deux manches sans lui : pas encore de compensation.
        let mut ev = Vec::new();
        while m.phase != phase::FREEZE {
            ev.extend(play(&mut m, 1.0));
        }
        ev.extend(next_round(&mut m));
        assert!(!says(&ev, "Compensation"), "{ev:?}");
        assert_eq!(m.players[&gone].missed, 2);
        // Troisième départ sans lui : 2500 $ partagés entre les 4 présents (650 chacun),
        // annoncé dans le tchat ; rien pour l'autre équipe.
        let before = m.players[&mate].money;
        let t_before: u32 = m
            .players
            .values()
            .filter(|p| p.team == T)
            .map(|p| p.money)
            .sum();
        let ev = next_round(&mut m);
        assert!(says(&ev, "+$650"), "{ev:?}");
        assert!(says(&ev, "1 joueur absent"));
        let _ = (before, t_before);
        // Un second joueur part : après ses deux manches, la compensation s'adapte
        // (2 absents, 3 présents : 850 par absent, 1700 chacun).
        m.leave(gone2, None);
        next_round(&mut m);
        next_round(&mut m);
        let ev = next_round(&mut m);
        assert!(
            says(&ev, "2 joueurs absents") && says(&ev, "+$1700"),
            "{ev:?}"
        );
        // Le premier revient pendant le temps de départ : sa part de cette manche est
        // reprise tout de suite, celle de l'autre absent reste.
        assert_eq!(m.phase, phase::FREEZE);
        let money = m.players[&mate].money;
        let ev = m.join(600, &names[0]);
        assert!(says(&ev, "-$850"), "{ev:?}");
        assert_eq!(m.players[&mate].money, money - 850);
        // Manche suivante : compensation pour un seul absent (2500 / 4 présents).
        let ev = next_round(&mut m);
        assert!(says(&ev, "1 joueur absent") && says(&ev, "+$650"), "{ev:?}");
        // Le second revient en pleine manche : rien n'est repris, et plus de
        // compensation à la manche suivante.
        play(&mut m, FREEZE_S + 1.0);
        assert_eq!(m.phase, phase::LIVE);
        let money = m.players[&mate].money;
        let ev = m.join(601, &names[1]);
        assert!(!says(&ev, "Compensation"));
        assert_eq!(m.players[&mate].money, money);
        kill_team(&mut m, T);
        let mut ev = Vec::new();
        while m.phase != phase::FREEZE {
            ev.extend(play(&mut m, 1.0));
        }
        assert!(!says(&ev, "Compensation"), "{ev:?}");
        // Exclu par un vote de son équipe : pas de compensation (l'équipe l'a voulu).
        m.players.get_mut(&cts[2]).unwrap().kicked = true;
        m.leave(cts[2], None);
        let mut ev = next_round(&mut m);
        ev.extend(next_round(&mut m));
        ev.extend(next_round(&mut m));
        assert!(!says(&ev, "Compensation"), "{ev:?}");
    }

    #[test]
    fn a_whole_team_can_surrender_and_the_other_wins() {
        let mut m = full_match();
        play(&mut m, FREEZE_S + 1.0);
        let ts: Vec<u64> = m
            .players
            .iter()
            .filter(|(_, p)| p.team == T)
            .map(|(&i, _)| i)
            .collect();
        // Proposé : il faut le oui de toute l'équipe ; un non suffit à refuser.
        m.call_vote(ts[0], VOTE_SURRENDER, u64::MAX).unwrap();
        for &t in &ts[1..4] {
            assert!(m.ballot(t, true).is_empty());
        }
        assert!(says(&m.ballot(ts[4], false), "refusé"));
        assert_eq!(m.surrendered, None);
        // Tous d'accord : la partie s'arrête, l'autre équipe gagne 13 à 0.
        m.call_vote(ts[1], VOTE_SURRENDER, u64::MAX).unwrap();
        let mut ev = Vec::new();
        for &t in &ts {
            ev.extend(m.ballot(t, true));
        }
        assert!(says(&ev, "abandonnent"));
        assert_eq!(m.phase, phase::GAME_OVER);
        let over = ev.iter().find_map(|e| match e {
            Ev::GameOver(s) => Some(s.clone()),
            _ => None,
        });
        let s = over.expect("fin de partie");
        // Les T ont commencé T (indice 1) : ils perdent.
        assert_eq!(s.score, [WIN, 0]);
        // Pas d'abandon pendant l'échauffement.
        let mut w = Match::new(FpsMap::default(), [0; 32], 7);
        w.join(1, "A");
        assert!(w.call_vote(1, VOTE_SURRENDER, u64::MAX).is_err());
    }

    fn full_match() -> Match {
        let mut m = Match::new(FpsMap::default(), [0; 32], 7);
        for i in 0..10u64 {
            m.join(i, &format!("J{i}"));
        }
        assert_eq!(m.count(CT), 5);
        assert_eq!(m.count(T), 5);
        let places = BTreeMap::new();
        // Compte à rebours de 10 s puis début.
        let mut started = false;
        for _ in 0..(12 * 128) {
            started |= m.tick(1.0 / 128.0, &places).contains(&Ev::Started);
        }
        assert!(started && m.phase == phase::FREEZE);
        m
    }

    #[test]
    fn items_are_tossed_within_reach_and_the_bomb_can_be_dropped() {
        let mut m = full_match();
        while m.phase == phase::FREEZE {
            m.tick(1.0 / 128.0, &BTreeMap::new());
        }
        let t = m.carrier.expect("un T a la bombe");
        let feet = Vec3::new(0.0, 10.0, 0.0);
        let far = feet + Vec3::new(9.0, 0.0, 0.0);
        let near = feet + Vec3::new(0.0, 0.0, -4.0);
        assert!(!m.toss(t, DROP_BOMB, feet, far), "trop loin : triche");
        assert!(m.toss(t, DROP_BOMB, feet, near));
        assert_eq!(m.bomb, bomb::DROPPED);
        assert_eq!(m.bomb_pos, near);
        assert!(m.carrier.is_none());
        assert!(!m.toss(t, DROP_BOMB, feet, near), "plus de bombe à lâcher");
        // Retombée à ses pieds : il ne la reprend pas tout de suite, un autre T oui.
        let place = |at: Vec3| Place {
            feet: at,
            on_ground: true,
            speed: 0.0,
        };
        let mut places = BTreeMap::new();
        places.insert(t, place(near));
        m.tick(1.0 / 128.0, &places);
        assert_eq!(m.bomb, bomb::DROPPED, "pas reprise aussitôt");
        let mate = m
            .players
            .iter()
            .find(|(&i, p)| i != t && p.team == T)
            .map(|(&i, _)| i)
            .unwrap();
        places.insert(mate, place(near));
        m.tick(1.0 / 128.0, &places);
        assert_eq!(m.carrier, Some(mate));
        // Grenade lancée : une de moins, au sol à l'arrivée, avec son point de départ.
        m.players.get_mut(&mate).unwrap().nades[0] = 1;
        assert!(m.toss(mate, ITEM_NADE as u8, feet, near));
        assert_eq!(m.players[&mate].nades[0], 0);
        let g = *m.ground.last().unwrap();
        assert!((Vec3::from(g.pos) - near).length() < 0.1);
        assert!(Vec3::from(g.from).y > feet.y + 1.0);
        assert!(
            !m.toss(mate, ITEM_NADE as u8, feet, near),
            "plus de grenade"
        );
    }

    fn kill_team(m: &mut Match, team: u8) {
        let ids: Vec<u64> = m
            .players
            .iter()
            .filter(|(_, p)| p.team == team && p.alive)
            .map(|(&i, _)| i)
            .collect();
        let killer = m
            .players
            .iter()
            .find(|(_, p)| p.team != team)
            .map(|(&i, _)| i);
        for id in ids {
            m.hurt(
                killer,
                id,
                500,
                Hurt::Bullet {
                    weapon: 0,
                    head: false,
                    legs: false,
                },
            );
        }
    }

    fn run(m: &mut Match, secs: f32) -> Vec<Ev> {
        let places = BTreeMap::new();
        let mut ev = Vec::new();
        for _ in 0..(secs * 128.0) as usize {
            ev.extend(m.tick(1.0 / 128.0, &places));
        }
        ev
    }

    #[test]
    fn economy_and_halves_follow_cs2() {
        let mut m = full_match();
        assert!(m.players.values().all(|p| p.money == 800));
        let t = *m.players.iter().find(|(_, p)| p.team == T).unwrap().0;
        let ct = *m.players.iter().find(|(_, p)| p.team == CT).unwrap().0;
        // Pas d'arme du camp adverse, pas de kit pour les T.
        let ak = WEAPONS.iter().position(|w| w.name == "AK-47").unwrap() as u16;
        assert!(m.price(ct, ak).is_none());
        assert!(m.price(t, ITEM_KIT).is_none());
        assert_eq!(m.price(ct, ITEM_KIT), Some(400));
        assert!(m.buy(ct, ITEM_KIT, None).is_ok());
        assert_eq!(m.players[&ct].money, 400);
        assert!(m.buy(ct, ITEM_HELMET, None).is_err(), "pas assez d'argent");
        // Manche 1 : les CT éliminent les T.
        run(&mut m, FREEZE_S + 0.1);
        assert_eq!(m.phase, phase::LIVE);
        kill_team(&mut m, T);
        run(&mut m, 0.1);
        assert_eq!(m.phase, phase::ROUND_END);
        assert_eq!(m.score_sides(), [1, 0]);
        assert_eq!(m.players[&t].money, 800 + 1400);
        let k = m.players[&ct].kills as u32;
        assert_eq!(m.players[&ct].money, 400 + 3250 + 300 * k);
        // Deuxième défaite : 1 900 $.
        run(&mut m, ROUND_END_S + FREEZE_S + 0.1);
        kill_team(&mut m, T);
        run(&mut m, 0.1);
        assert_eq!(m.players[&t].money, 800 + 1400 + 1900);
        // Jusqu'à la mi-temps : changement de côté et 800 $.
        for _ in 2..12 {
            run(&mut m, ROUND_END_S + FREEZE_S + 0.1);
            kill_team(&mut m, T);
            run(&mut m, 0.1);
        }
        run(&mut m, ROUND_END_S + 0.1);
        assert_eq!(m.phase, phase::HALFTIME);
        assert_eq!(m.players[&t].team, CT, "côtés échangés");
        assert!(m.players.values().all(|p| p.money == 800));
        assert_eq!(
            m.score_sides(),
            [0, 12],
            "l'équipe gagnante est maintenant T"
        );
        // Une manche de plus pour elle : 13, fin.
        run(&mut m, HALFTIME_S + FREEZE_S + 0.1);
        kill_team(&mut m, CT);
        let ev = run(&mut m, ROUND_END_S + 0.5);
        let over = ev.iter().find_map(|e| match e {
            Ev::GameOver(s) => Some(s.clone()),
            _ => None,
        });
        let s = over.expect("fin de partie");
        assert_eq!(s.score, [13, 0]);
        assert_eq!(s.players.len(), 10);
    }

    #[test]
    fn bomb_is_planted_on_a_site_and_defused_faster_with_a_kit() {
        let mut map = FpsMap::default();
        map.sites[1] = Some(Zone::new([10, 0, 10], [14, 3, 14]));
        let mut m = Match::new(map, [0; 32], 3);
        for i in 0..10u64 {
            m.join(i, &format!("J{i}"));
        }
        run(&mut m, 11.0 + FREEZE_S + 0.1);
        assert_eq!(m.phase, phase::LIVE);
        let c = m.carrier.expect("un T a la bombe");
        let ct = *m.players.iter().find(|(_, p)| p.team == CT).unwrap().0;
        m.players.get_mut(&ct).unwrap().kit = true;
        let mut places = BTreeMap::new();
        let on = |x: f32| Place {
            feet: Vec3::new(x, 1.0, 12.0),
            on_ground: true,
            speed: 0.0,
        };
        places.insert(c, on(3.0));
        m.keys(c, true, false);
        for _ in 0..(4 * 128) {
            m.tick(1.0 / 128.0, &places);
        }
        assert_eq!(m.bomb, bomb::CARRIED, "hors site : rien");
        places.insert(c, on(12.0));
        for _ in 0..(3 * 128) {
            m.tick(1.0 / 128.0, &places);
        }
        assert_eq!(m.bomb, bomb::CARRIED, "3 s < 3,2 s");
        for _ in 0..40 {
            m.tick(1.0 / 128.0, &places);
        }
        assert_eq!(m.bomb, bomb::PLANTED);
        assert_eq!(m.site, 1);
        // Les T meurent : la manche continue (bombe posée).
        kill_team(&mut m, T);
        places.remove(&c);
        m.tick(0.01, &places);
        assert_eq!(m.phase, phase::PLANTED);
        places.insert(ct, on(12.5));
        m.keys(ct, false, true);
        let mut ev = Vec::new();
        for _ in 0..(5 * 128 + 10) {
            ev.extend(m.tick(1.0 / 128.0, &places));
        }
        assert!(ev.contains(&Ev::RoundEnd {
            winner: CT,
            reason: reason::DEFUSED
        }));
    }

    #[test]
    fn overtime_needs_four_in_a_block() {
        let mut m = Match::new(FpsMap::default(), [0; 32], 1);
        m.wins = [12, 12];
        assert!(!m.finished());
        m.wins = [15, 13];
        assert!(!m.finished());
        m.wins = [16, 13];
        assert!(m.finished());
        m.wins = [15, 15];
        assert!(!m.finished());
        m.wins = [19, 17];
        assert!(m.finished());
    }

    #[test]
    fn buying_only_in_ones_own_spawn() {
        let map = FpsMap {
            ct: Some(Zone::new([0, 0, 0], [4, 3, 4])),
            t: Some(Zone::new([50, 0, 50], [54, 3, 54])),
            ..Default::default()
        };
        let mut m = Match::new(map, [0; 32], 1);
        m.join(1, "A");
        let team = m.players[&1].team;
        let (home, away) = if team == CT {
            (Vec3::new(2.0, 1.0, 2.0), Vec3::new(52.0, 1.0, 52.0))
        } else {
            (Vec3::new(52.0, 1.0, 52.0), Vec3::new(2.0, 1.0, 2.0))
        };
        assert!(m.can_buy(1, Some(home)));
        assert!(!m.can_buy(1, Some(away)), "pas dans le départ adverse");
        assert!(
            !m.can_buy(1, Some(Vec3::new(25.0, 1.0, 25.0))),
            "pas au milieu"
        );
        let mut m = Match::new(FpsMap::default(), [0; 32], 1);
        m.fallback_spawns = Some([Vec3::new(-10.0, 0.0, 0.0), Vec3::new(10.0, 0.0, 0.0)]);
        m.join(1, "A");
        let x = if m.players[&1].team == CT {
            -10.0
        } else {
            10.0
        };
        assert!(m.can_buy(1, Some(Vec3::new(x, 0.0, 1.0))));
        assert!(!m.can_buy(1, Some(Vec3::new(x + 30.0, 0.0, 1.0))));
    }

    #[test]
    fn armor_absorbs_part_of_the_damage() {
        let mut m = Match::new(FpsMap::default(), [0; 32], 1);
        m.join(1, "A");
        m.join(2, "B");
        if m.players[&1].team == m.players[&2].team {
            m.players.get_mut(&2).unwrap().team = other(m.players[&1].team);
        }
        m.players.get_mut(&2).unwrap().armor = 100;
        let ak = WEAPONS.iter().position(|w| w.name == "AK-47").unwrap() as u8;
        m.hurt(
            Some(1),
            2,
            36,
            Hurt::Bullet {
                weapon: ak,
                head: false,
                legs: false,
            },
        );
        assert_eq!(m.players[&2].hp, 100 - 28, "36 × 0,775 ≈ 28");
        assert!(m.players[&2].armor < 100);
        // Tête sans casque : tout passe.
        m.hurt(
            Some(1),
            2,
            40,
            Hurt::Bullet {
                weapon: ak,
                head: true,
                legs: false,
            },
        );
        assert_eq!(m.players[&2].hp, 72 - 40);
    }
}
