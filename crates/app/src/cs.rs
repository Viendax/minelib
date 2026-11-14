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

use std::collections::HashMap;

use glam::Vec3;
use minelib_net::game::{
    bomb, phase, reason, MatchHud, MatchSummary, ScoreRow, SummaryPlayer, Zone, CT, ITEM_HELMET,
    ITEM_KEVLAR, ITEM_KIT, ITEM_NADE, T,
};

use crate::weapons::WEAPONS;

pub const PLAYERS: usize = 10;
pub const TEAM_MAX: usize = 5;
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
const WIN_ELIM: u32 = 3_250;
const WIN_BOMB: u32 = 3_500;
const LOSS_BASE: u32 = 1_400;
const LOSS_STEP: u32 = 500;
const PLANT_TEAM_BONUS: u32 = 800;
const OBJECTIVE_REWARD: u32 = 300;
const TEAMKILL_PENALTY: u32 = 300;
pub const KIT_PRICE: u32 = 400;
pub const KEVLAR_PRICE: u32 = 650;
pub const HELMET_PRICE: u32 = 1_000;
/// Distance (blocs) pour ramasser la bombe, et pour la désamorcer.
const PICKUP: f32 = 1.2;
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
    dealt: HashMap<u64, u32>,
    dead_for: f32,
    /// Touches maintenues : poser, désamorcer.
    pub plant_key: bool,
    pub defuse_key: bool,
    /// Il faudra repartir avec le pistolet de départ (mort pendant la manche).
    pub reset: bool,
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
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Match {
    pub players: HashMap<u64, MPlayer>,
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
            players: HashMap::new(),
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
            site: 0,
            planter: None,
            plant_progress: 0.0,
            defuser: None,
            defuse_progress: 0.0,
            live_for: 0.0,
            map,
            map_sig,
            rng: seed | 1,
            planted_this_round: false,
            countdown: false,
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

    /// Un joueur arrive : CT ou T au hasard, 5 par équipe au plus.
    pub fn join(&mut self, id: u64, pseudo: &str) -> Vec<Ev> {
        let (ct, t) = (self.count(CT), self.count(T));
        let team = if ct >= TEAM_MAX {
            T
        } else if t >= TEAM_MAX {
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
                dealt: HashMap::new(),
                dead_for: 0.0,
                plant_key: false,
                defuse_key: false,
                reset: true,
            },
        );
        let side = if team == CT { "CT" } else { "T" };
        let mut ev = vec![Ev::Say(format!("{pseudo} rejoint les {side}."))];
        if !self.started() {
            ev.push(Ev::Spawn { id, reset: true });
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
        }
    }

    /// Échauffement : le joueur change d'équipe (s'il y a de la place).
    pub fn switch_team(&mut self, id: u64, team: u8) -> Option<Ev> {
        if self.started() || team > T || self.count(team) >= TEAM_MAX {
            return None;
        }
        let p = self.players.get_mut(&id)?;
        if p.team == team {
            return None;
        }
        p.team = team;
        p.start_team = team;
        Some(Ev::Spawn { id, reset: true })
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
            _ => true,
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
                Some(crate::equip::Nade::ALL[(n - ITEM_NADE) as usize].price())
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
            _ => {}
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
                    ev.push(Ev::Say(format!(
                        "{} a tué un coéquipier (−300 $).",
                        kp.pseudo
                    )));
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
    pub fn tick(&mut self, dt: f32, places: &HashMap<u64, Place>) -> Vec<Ev> {
        let mut ev = Vec::new();
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
            ev.push(Ev::Spawn { id, reset: true });
        }
        let full = self.count(CT) >= TEAM_MAX && self.count(T) >= TEAM_MAX;
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
                "10 joueurs : la partie commence dans {} s.",
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
            ev.push(Ev::Spawn { id: *id, reset });
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
        ev.push(Ev::NewRound);
        ev.push(Ev::Say(format!("Manche {}.", self.played + 1)));
    }

    fn tick_bomb(&mut self, dt: f32, places: &HashMap<u64, Place>, ev: &mut Vec<Ev>) {
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
                // Un T vivant qui passe dessus la ramasse.
                let taker = places
                    .iter()
                    .filter(|(id, _)| {
                        self.players
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

    fn check_round(&mut self, ev: &mut Vec<Ev>, _places: &HashMap<u64, Place>) {
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
            })
            .collect();
        players.sort_by(|a, b| a.pseudo.cmp(&b.pseudo));
        MatchSummary {
            map_sig: self.map_sig,
            score: self.wins,
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
            bomb_pos: self.bomb_pos.to_array(),
            progress: progress.clamp(0.0, 1.0),
            can_buy: self.can_buy(id, at),
            alive: [self.alive(CT) as u8, self.alive(T) as u8],
            site: self.site,
            players: self.present() as u8,
        }
    }

    pub fn rows(&self) -> Vec<ScoreRow> {
        let mut v: Vec<ScoreRow> = self
            .players
            .iter()
            .filter(|(_, p)| p.present || self.started())
            .map(|(&id, p)| ScoreRow {
                id,
                pseudo: p.pseudo.clone(),
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

    fn full_match() -> Match {
        let mut m = Match::new(FpsMap::default(), [0; 32], 7);
        for i in 0..10u64 {
            m.join(i, &format!("J{i}"));
        }
        assert_eq!(m.count(CT), 5);
        assert_eq!(m.count(T), 5);
        let places = HashMap::new();
        // Compte à rebours de 10 s puis début.
        let mut started = false;
        for _ in 0..(12 * 128) {
            started |= m.tick(1.0 / 128.0, &places).contains(&Ev::Started);
        }
        assert!(started && m.phase == phase::FREEZE);
        m
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
        let places = HashMap::new();
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
        let mut places = HashMap::new();
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
