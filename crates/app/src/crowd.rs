//! Essai de foule (temporaire, v164) : en solo, des joueurs simulés apparaissent autour du
//! joueur, de 0 à 4 000 en 2 minutes de jeu, et se promènent. Ils sont dessinés exactement
//! comme les joueurs d'un serveur de construction (mêmes modèles, mêmes paliers de détail,
//! mêmes pseudos au-dessus de la tête), pour mesurer la fluidité qu'aurait une vraie foule
//! autour de soi. (v185) Éteint par défaut : `/npc stresstest true` le lance,
//! `/npc stresstest false` les retire.
//!
//! (v226) On peut les frapper (clic gauche à portée) : le PNJ recule d'un bond, rougit et
//! s'enfuit en courant. Un personnage caché, Pistache (skin dessiné pour lui, pseudo de la
//! foule comme les autres), s'y promène : celui qui le trouve et le frappe est prévenu, et
//! il repart se cacher ailleurs.
//!
//! (v238) Ils ont la vie d'un joueur (20 points ; un coup en ôte un, la foudre cinq, une chute
//! de plus de 4 blocs aussi) : tués, ils réapparaissent ailleurs dans la foule. Ils se
//! déplacent comme un joueur : ils sautent sur une marche d'un bloc, tombent d'un rebord (et
//! s'y font mal), nagent en surface dans l'eau ; un mur de deux blocs les fait tourner.
//!
//! (v227) En construction en ligne, un OP peut la lancer : son jeu la mène (`Crowd`) et dit
//! aux joueurs proches, par le relais, ce qui change (`Outbox`, paquets de
//! `minelib_net::crowd`) ; chez eux, elle avance de même entre deux nouvelles
//! (`RemoteCrowd`). Ils peuvent frapper ses PNJ (le coup part au jeu de l'OP) et trouver
//! Pistache : les joueurs proches en sont prévenus.

use glam::Vec3;
use minelib_net::crowd::{NpcState, Packet};
use minelib_world::World;

/// Joueurs simulés au plus, et temps (s) pour y arriver depuis zéro.
pub const MAX: usize = 4000;
const RAMP_S: f32 = 120.0;
/// Ils apparaissent entre ces deux distances du joueur (blocs) ; plus loin que `GONE`, ils
/// réapparaissent près de lui.
const NEAR: f32 = 6.0;
const FAR: f32 = 150.0;
const GONE: f32 = 200.0;
/// Le journal note leur nombre toutes les… (s), à côté des bilans d'images par seconde.
const LOG_EVERY: f32 = 10.0;
/// Joueurs dont on reprend l'abri par image (voir `Npc::covered`).
const RECHECK_PER_FRAME: usize = 64;
/// (v234) PNJ partis trop loin (`GONE`) qu'on essaie de ramener par image. Avant, tous
/// cherchaient un sol à chaque image : en volant loin de la foule (plus de sol à moins de
/// 40 blocs, ou des chunks pas chargés), les 4 000 échouaient et recommençaient à chaque
/// image, soit un million de blocs lus (« logique et physique » à 11 ms, 77 FPS).
const RESPAWNS_PER_FRAME: usize = 64;
/// (v226) Coup reçu : recul (blocs/s, à l'horizontale, puis vers le haut), pesanteur
/// (blocs/s²). (v234) « Hauteur gagnée en étant tapé / 3 » (à force de clics, on les faisait
/// monter dans le ciel) : vers le haut, 5,5 / √3 au lieu de 5,5, un bond d'un sixième de bloc
/// de haut au lieu d'un demi, à un bloc en arrière. Pour les garder en l'air, il faut
/// maintenant près de 5 coups par seconde au lieu de 3, et ils montent bien moins vite.
/// (v247) « La hauteur gagnée au recul doit être deux fois moins élevée » : 3,175 / √2 (la
/// hauteur va comme le carré de la vitesse), un bond d'un douzième de bloc.
const KNOCK: f32 = 5.0;
const KNOCK_UP: f32 = 3.175 * std::f32::consts::FRAC_1_SQRT_2;
/// (v238) Pesanteur : celle d'un joueur (`player::GRAVITY`, 30 avant).
const GRAVITY: f32 = 28.0;
/// (v238) Vie d'un PNJ : celle d'un joueur. Un coup en ôte `PUNCH` (comme un coup de poing
/// sur un joueur), la foudre `lightning::DAMAGE`, une chute ce qu'elle ôte à un joueur.
pub const NPC_HP: f32 = 20.0;
pub const PUNCH: f32 = 1.0;
/// (v238) Saut d'un PNJ sur une marche d'un bloc (blocs/s) : celui d'un joueur
/// (`player::JUMP_SPEED`).
const JUMP_UP: f32 = 8.6;
/// (v238) Chute sans dégâts (blocs), comme un joueur (`session::SAFE_FALL`).
const SAFE_FALL: f32 = 4.0;
/// (v238) Dans l'eau, il nage en surface : ses pieds à cette profondeur sous la surface, à
/// cette part de sa vitesse.
const SWIM_DEPTH: f32 = 1.3;
const SWIM_SPEED: f32 = 0.5;
/// (v238) Tué, il réapparaît au moins à cette distance de qui l'a tué (blocs).
const RESPAWN_AWAY: f32 = 24.0;
/// (v226) Durée du rouge d'un coup reçu, et de la fuite en courant qui suit (s).
pub const HURT_S: f32 = 0.4;
const FLEE_S: f32 = 3.0;
const RUN: f32 = 5.6;
/// (v226) Le personnage caché se cache au moins à cette distance de qui le cherche (blocs).
const HIDE_MIN: f32 = 40.0;
/// (v226) Son nom, dit à qui le trouve.
pub const HIDDEN_NAME: &str = "Pistache";
/// (v227) Foule menée en ligne : un paquet toutes les… (s), et chaque PNJ redit au moins
/// toutes les… (s) (pour ceux qui arrivent) ; PNJ par paquet au plus.
pub const PACKET_EVERY: f32 = 0.05;
pub const REFRESH_S: f32 = 4.0;
const PACKET_MAX: usize = 1200;
/// (v227) Une foule reçue dont on n'a plus de nouvelles depuis… (s) est oubliée (celui qui la
/// menait est parti, ou trop loin).
pub const REMOTE_TIMEOUT: f32 = 5.0;

/// Un joueur simulé.
pub struct Npc {
    pub name: String,
    pub pos: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    /// Vitesse au sol (blocs/s : arrêté, marche ou course).
    speed: f32,
    /// Temps avant de changer de direction ou d'allure.
    turn_in: f32,
    /// Case (x, z) dont la hauteur du sol est connue : on ne la recherche qu'en changeant
    /// de case (des milliers de joueurs : pas de recherche à chaque image).
    cell: (i32, i32),
    /// (v194) Sa marche (balancement des bras et des jambes, pose de l'image d'avant) :
    /// portée par lui, et non cherchée par son pseudo dans une table — à 4 000 joueurs
    /// c'était 8 000 recherches par image, deux fois le hachage de chaque pseudo.
    pub walk: crate::session::Walk,
    /// (v197) Son skin (presque toujours aucun : le modèle par défaut), repris de temps en
    /// temps comme `covered` — et non cherché par son pseudo à chaque image.
    pub skin: Option<std::sync::Arc<crate::skin::Skin>>,
    /// (v191) Le ciel est-il bouché au-dessus de lui ? Sa lumière en découle. Cherché en
    /// changeant de case, et repris pour quelques-uns à chaque image (un toit peut être
    /// construit au-dessus d'eux) : parcourir la colonne de blocs de 4 000 joueurs à
    /// chaque image coûtait près d'une demi-milliseconde.
    pub covered: bool,
    /// (v226) Bond d'un coup reçu : sa vitesse (blocs/s), et depuis combien de temps il est
    /// en l'air (0 : au sol).
    push: Vec3,
    air: f32,
    /// (v226) Temps qui reste du rouge d'un coup reçu (s).
    pub hurt: f32,
    /// (v226) C'est Pistache, le personnage caché (un seul dans la foule).
    pub hidden: bool,
    /// (v227) Numéro de son pseudo (`Pnj` suivi de ce numéro), dit aux autres joueurs.
    num: u32,
    /// (v227) Foule reçue : le dernier état reçu le disait-il frappé ? (Son cri : seulement
    /// quand il vient de l'être.)
    heard_hurt: bool,
    /// (v238) Points de vie (`NPC_HP` au départ).
    pub hp: f32,
    /// (v238) En l'air : le plus haut de sa chute (dégâts à l'arrivée).
    top: f32,
    /// (v238) Dans l'eau : il nage (en surface, plus lentement).
    swim: bool,
    /// (v238) En l'air sur un saut : un mur devant l'arrête sans lui ôter son élan (il
    /// passe dessus une fois assez haut).
    jumping: bool,
}

pub struct Crowd {
    pub npcs: Vec<Npc>,
    t: f32,
    seed: u32,
    log_in: f32,
    /// Numéro du prochain pseudo.
    next: usize,
    /// Prochain joueur dont on reprendra l'abri (voir `Npc::covered`).
    recheck: usize,
    /// (v226) Le personnage caché est-il dans la foule ? (Le premier venu assez loin.)
    hidden_in: bool,
    /// (v227) PNJ qui viennent de changer (apparu, tourné, arrêté, frappé, retombé, reparti
    /// ailleurs) : ce que le jeu qui la mène en ligne dit tout de suite (`Outbox`). Vidé à
    /// chaque image.
    pub changed: Vec<u16>,
    /// (v299) PNJ qui viennent de mourir (avant de repartir d'ailleurs) : où ils étaient, leur
    /// cap, leur skin, et d'où venait le coup. Le jeu y laisse leur corps (`corpse::Fallen`).
    /// Vidé par la session.
    pub fallen: Vec<(Vec3, f32, Option<std::sync::Arc<crate::skin::Skin>>, Vec3)>,
    /// (v234) PNJ partis trop loin qu'on peut encore essayer de ramener à cette image.
    respawns: usize,
    /// (v234) Essais : recherches de sol faites (`spawn`).
    #[cfg(test)]
    tries: usize,
}

/// (v226) Ce qu'a fait un coup (`Crowd::hit`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    /// Un PNJ de la foule : il recule et s'enfuit.
    Npc,
    /// (v238) Ce coup l'a tué : il est reparti d'ailleurs, avec toute sa vie.
    Killed,
    /// Le personnage caché : trouvé, il est reparti se cacher ailleurs.
    Hidden,
}

impl Default for Crowd {
    fn default() -> Self {
        Self::new()
    }
}

impl Crowd {
    pub fn new() -> Self {
        Self {
            npcs: Vec::new(),
            t: 0.0,
            seed: 0x9e37_79b9,
            log_in: LOG_EVERY,
            next: 1,
            recheck: 0,
            hidden_in: false,
            changed: Vec::new(),
            fallen: Vec::new(),
            respawns: 0,
            #[cfg(test)]
            tries: 0,
        }
    }

    /// Nombre de joueurs voulu après `t` secondes d'essai.
    pub fn wanted(t: f32) -> usize {
        ((t / RAMP_S).clamp(0.0, 1.0) * MAX as f32) as usize
    }

    fn rnd(&mut self) -> f32 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 17;
        self.seed ^= self.seed << 5;
        (self.seed >> 8) as f32 / (1u32 << 24) as f32
    }

    /// Une image : de nouveaux joueurs autour de `center` (jusqu'au nombre voulu), et
    /// chacun avance.
    pub fn update(
        &mut self,
        world: &World,
        skins: &crate::skin::Skins,
        center: Vec3,
        dt: f32,
    ) {
        self.t += dt;
        let want = Self::wanted(self.t);
        // Quelques apparitions par image au plus (la recherche du sol coûte).
        let mut budget = 64;
        while self.npcs.len() < want && budget > 0 {
            budget -= 1;
            if let Some(mut n) = self.spawn(world, center) {
                // (v226) Le personnage caché : le premier venu assez loin.
                if !self.hidden_in && n.pos.distance(center) >= HIDE_MIN {
                    self.hidden_in = true;
                    n.hidden = true;
                    n.skin = Some(crate::bot_skin::hidden_skin());
                }
                self.npcs.push(n);
                self.changed.push((self.npcs.len() - 1) as u16);
            }
        }
        self.respawns = RESPAWNS_PER_FRAME;
        for i in 0..self.npcs.len() {
            self.step(world, center, i, dt);
        }
        // Leur marche, dans la foulée (voir `Npc::walk`).
        for n in &mut self.npcs {
            let (pos, yaw, pitch) = (n.pos, n.yaw, n.pitch);
            n.walk.update(pos, yaw, pitch, 0.0, n.air > 0.0, dt);
        }
        // Abri repris pour quelques joueurs par image : tous en une minute environ.
        for _ in 0..RECHECK_PER_FRAME.min(self.npcs.len()) {
            self.recheck = (self.recheck + 1) % self.npcs.len();
            let i = self.recheck;
            // (v344) Sans le demander au relais : un joueur simulé n'a pas de compte.
            let skin = skins.cached(&self.npcs[i].name);
            let n = &mut self.npcs[i];
            n.covered = covered(world, n.pos);
            // (Le personnage caché garde le sien.)
            if !n.hidden {
                n.skin = skin;
            }
        }
        self.log_in -= dt;
        if self.log_in <= 0.0 {
            self.log_in = LOG_EVERY;
            log::info!(
                "foule d'essai : {} joueurs simulés autour de toi (vers {MAX} en {RAMP_S:.0} s)",
                self.npcs.len()
            );
        }
    }

    /// Un joueur posé sur le sol, à une distance au hasard de `center`.
    fn spawn(&mut self, world: &World, center: Vec3) -> Option<Npc> {
        #[cfg(test)]
        {
            self.tries += 1;
        }
        let a = self.rnd() * std::f32::consts::TAU;
        // Réparti sur la surface du disque (autant de monde loin que près, par bloc²).
        let r = NEAR + (FAR - NEAR) * self.rnd().sqrt();
        let (x, z) = (center.x + a.cos() * r, center.z + a.sin() * r);
        let y = surface(world, x, z, center.y)?;
        let num = self.next as u32;
        let name = format!("Pnj{num}");
        self.next += 1;
        let yaw = self.rnd() * std::f32::consts::TAU;
        let speed = self.pick_speed();
        let turn_in = 1.0 + self.rnd() * 6.0;
        let pos = Vec3::new(x, y, z);
        Some(Npc {
            name,
            walk: crate::session::Walk::new(pos),
            skin: None,
            pos,
            yaw,
            pitch: 0.0,
            speed,
            turn_in,
            cell: (x.floor() as i32, z.floor() as i32),
            covered: covered(world, pos),
            push: Vec3::ZERO,
            air: 0.0,
            hurt: 0.0,
            hidden: false,
            num,
            heard_hurt: false,
            hp: NPC_HP,
            top: y,
            swim: false,
            jumping: false,
        })
    }

    /// (v226) PNJ dont la boîte (0,6 × 1,8 × 0,6 bloc, comme un joueur) est touchée par le
    /// rayon `eye` + t · `dir` à moins de `reach` blocs : le plus proche, et sa distance.
    pub fn aimed(&self, eye: Vec3, dir: Vec3, reach: f32) -> Option<(usize, f32)> {
        aimed_in(&self.npcs, eye, dir, reach)
    }

    /// (v226) Coup de celui dont les yeux sont en `from` au PNJ `i` : il recule d'un bond loin
    /// de lui, rougit, puis s'enfuit en courant. Le personnage caché, lui, est trouvé : il
    /// repart se cacher ailleurs dans la foule (autour de `center`, loin de `from`), sous un
    /// autre pseudo.
    pub fn hit(&mut self, world: &World, i: usize, from: Vec3, center: Vec3) -> Hit {
        self.hit_for(world, i, from, center, PUNCH)
    }

    /// (v241) Le même coup, qui ôte `amount` points de vie (l'arc court de la Foudre de Zeus
    /// sans charge : `zeus::ARC_DAMAGE`).
    pub fn hit_for(&mut self, world: &World, i: usize, from: Vec3, center: Vec3, amount: f32) -> Hit {
        self.changed.push(i as u16);
        if self.npcs[i].hidden {
            self.hide(world, i, center, from);
            return Hit::Hidden;
        }
        self.npcs[i].knock(from);
        // (v238) Un point de vie de moins ; plus rien : il réapparaît ailleurs.
        if self.hurt(world, i, amount, center, from) {
            return Hit::Killed;
        }
        Hit::Npc
    }

    /// (v238) Le PNJ `i` perd `amount` points de vie ; à zéro, il réapparaît ailleurs dans la
    /// foule (autour de `center`, à `RESPAWN_AWAY` blocs au moins de `from`), avec toute sa
    /// vie. Rend vrai s'il en est mort. (Le personnage caché ne meurt pas.)
    fn hurt(&mut self, world: &World, i: usize, amount: f32, center: Vec3, from: Vec3) -> bool {
        let n = &mut self.npcs[i];
        if n.hidden {
            return false;
        }
        n.hp -= amount;
        n.hurt = HURT_S;
        n.heard_hurt = true;
        if n.hp > 0.0 {
            return false;
        }
        self.changed.push(i as u16);
        if self.fallen.len() < 8 {
            let n = &self.npcs[i];
            self.fallen.push((n.pos, n.yaw, n.skin.clone(), from));
        }
        self.respawn(world, i, center, from);
        true
    }

    /// (v238) Le PNJ `i` repart d'ailleurs (mort) : un endroit de la foule autour de `center`,
    /// loin de `from` (sinon le premier trouvé), avec toute sa vie et le même pseudo. Reste où
    /// il est, sa vie refaite, s'il n'en trouve pas (chunks pas chargés).
    fn respawn(&mut self, world: &World, i: usize, center: Vec3, from: Vec3) {
        let (mut found, mut near) = (None, None);
        for _ in 0..16 {
            let Some(m) = self.spawn(world, center) else {
                continue;
            };
            // (Le pseudo pris par `spawn` n'est pas utilisé.)
            self.next -= 1;
            if m.pos.distance(from) >= RESPAWN_AWAY {
                found = Some(m);
                break;
            }
            near = Some(m);
        }
        let found = found.or(near);
        let n = &mut self.npcs[i];
        match found {
            Some(m) => {
                let name = std::mem::take(&mut n.name);
                let (num, skin) = (n.num, n.skin.take());
                *n = Npc {
                    name,
                    num,
                    skin,
                    ..m
                };
            }
            None => {
                n.hp = NPC_HP;
                n.hurt = 0.0;
            }
        }
    }

    /// (v238) La foudre tombée en `at` : les PNJ qu'elle touche (comme un joueur, voir
    /// `lightning::hurts_at`) perdent `lightning::DAMAGE` points et sautent loin d'elle (ceux
    /// qui en meurent réapparaissent ailleurs). Rend où ils étaient. (v247) `mine` : notre
    /// foudre, qui frappe comme nous : le personnage caché qu'elle touche est trouvé (et part
    /// se cacher ailleurs) ; rendu vrai alors.
    pub fn struck(&mut self, world: &World, at: Vec3, center: Vec3, mine: bool) -> (Vec<Vec3>, bool) {
        let mut hit = Vec::new();
        let mut found = false;
        for i in 0..self.npcs.len() {
            let n = &self.npcs[i];
            if !crate::lightning::hurts_at(at, n.pos) {
                continue;
            }
            if n.hidden {
                if mine && !found {
                    found = true;
                    self.hide(world, i, center, at);
                }
                continue;
            }
            hit.push(n.pos);
            self.changed.push(i as u16);
            self.npcs[i].knock(at);
            self.hurt(world, i, crate::lightning::DAMAGE as f32, center, at);
        }
        (hit, found)
    }

    /// (v226) Le personnage caché `i` part se cacher ailleurs : à un endroit de la foule
    /// (autour de `center`) à au moins `HIDE_MIN` blocs de `away_from`, sous un pseudo neuf.
    /// Reste où il est s'il n'en trouve pas (chunks pas chargés).
    fn hide(&mut self, world: &World, i: usize, center: Vec3, away_from: Vec3) {
        self.changed.push(i as u16);
        for _ in 0..32 {
            let Some(m) = self.spawn(world, center) else {
                continue;
            };
            if m.pos.distance(away_from) < HIDE_MIN {
                continue;
            }
            let skin = self.npcs[i].skin.clone();
            self.npcs[i] = Npc {
                hidden: true,
                skin,
                ..m
            };
            return;
        }
    }

    /// Arrêté (un sur cinq), à la marche ou en courant.
    fn pick_speed(&mut self) -> f32 {
        let r = self.rnd();
        if r < 0.2 {
            0.0
        } else if r < 0.75 {
            4.3
        } else {
            5.6
        }
    }

    fn step(&mut self, world: &World, center: Vec3, i: usize, dt: f32) {
        let r1 = self.rnd();
        let r2 = self.rnd();
        let n = &mut self.npcs[i];
        n.hurt = (n.hurt - dt).max(0.0);
        // (v226) En l'air après un coup : il retombe, puis s'enfuit. (v238) Ou après un saut,
        // ou d'un rebord : une chute de plus de `SAFE_FALL` blocs lui fait mal.
        if n.air > 0.0 {
            let (jumped, from) = (n.jumping, n.cell);
            if let Some(fall) = fly(world, n, dt) {
                self.changed.push(i as u16);
                // (v238) Un saut qui n'a pas passé la marche (retombé dans sa case) : il
                // fait demi-tour, sinon il ressauterait sans fin.
                let n = &mut self.npcs[i];
                if jumped && n.cell == from {
                    n.yaw = (n.yaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU);
                }
                if fall > SAFE_FALL {
                    let at = self.npcs[i].pos;
                    self.hurt(world, i, (fall - SAFE_FALL).ceil(), center, at);
                }
            }
            return;
        }
        n.turn_in -= dt;
        let turn = n.turn_in <= 0.0;
        if turn {
            self.changed.push(i as u16);
            n.turn_in = 1.0 + r1 * 6.0;
            n.yaw = (n.yaw + (r2 - 0.5) * 2.5).rem_euclid(std::f32::consts::TAU);
        }
        let far = (n.pos - center).length() > GONE;
        let hidden = n.hidden;
        if far {
            // (v234) Pas plus de `RESPAWNS_PER_FRAME` essais par image : les autres attendent
            // (loin, invisibles, sans marcher).
            if self.respawns == 0 {
                return;
            }
            self.respawns -= 1;
        }
        if far && hidden {
            // (v226) Le personnage caché revient aussi, mais pas tout près.
            self.hide(world, i, center, center);
            return;
        }
        if far {
            // Trop loin : il revient près du joueur.
            if let Some(m) = self.spawn(world, center) {
                let name = std::mem::take(&mut self.npcs[i].name);
                let num = self.npcs[i].num;
                self.npcs[i] = Npc { name, num, ..m };
                self.changed.push(i as u16);
                // (v194) Il repart d'ailleurs : sa marche aussi, sinon il arriverait en
                // courant (et laisserait une traînée d'anticrénelage sur tout le chemin).
                // (Le pseudo pris par `spawn` n'est pas utilisé.)
                self.next -= 1;
            }
            return;
        }
        if turn {
            let s = self.pick_speed();
            self.npcs[i].speed = s;
        }
        let n = &mut self.npcs[i];
        if n.speed <= 0.0 {
            return;
        }
        let (sy, cy) = n.yaw.sin_cos();
        let dir = Vec3::new(sy, 0.0, -cy);
        // (v238) Dans l'eau, il nage, plus lentement.
        let speed = n.speed * if n.swim { SWIM_SPEED } else { 1.0 };
        let next = n.pos + dir * speed * dt;
        let cell = (next.x.floor() as i32, next.z.floor() as i32);
        if cell == n.cell {
            n.pos = next;
            return;
        }
        // Nouvelle case : (v238) comme un joueur. Même sol (ou une marche plus bas) : il y
        // va ; un bloc plus haut : il saute ; plus bas : il tombe ; de l'eau : il nage en
        // surface (et en ressort sur une berge d'un bloc au plus) ; un mur : demi-tour.
        match ahead(world, n.pos, next.x, next.z, n.swim) {
            Ahead::Walk(y) => {
                n.pos = Vec3::new(next.x, y, next.z);
                n.cell = cell;
                n.swim = false;
                n.covered = covered(world, n.pos);
            }
            Ahead::Swim(y) => {
                n.pos = Vec3::new(next.x, y, next.z);
                n.cell = cell;
                n.swim = true;
                n.covered = covered(world, n.pos);
            }
            Ahead::Jump => {
                n.push = dir * speed + Vec3::Y * JUMP_UP;
                n.air = 1e-3;
                n.top = n.pos.y;
                n.jumping = true;
                self.changed.push(i as u16);
            }
            Ahead::Fall => {
                n.pos = Vec3::new(next.x, n.pos.y, next.z);
                n.cell = cell;
                n.push = dir * speed;
                n.air = 1e-3;
                n.top = n.pos.y;
                n.swim = false;
                self.changed.push(i as u16);
            }
            Ahead::Wall => {
                n.yaw = (n.yaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU);
                self.changed.push(i as u16);
            }
        }
    }
}

impl Npc {
    /// (v226) Coup reçu de celui dont les yeux sont en `from` : un bond en arrière, le rouge,
    /// puis la fuite, droit devant lui, loin du coup.
    fn knock(&mut self, from: Vec3) {
        let mut away =
            Vec3::new(self.pos.x - from.x, 0.0, self.pos.z - from.z).normalize_or_zero();
        if away == Vec3::ZERO {
            away = Vec3::X;
        }
        self.push = away * KNOCK + Vec3::Y * KNOCK_UP;
        self.air = self.air.max(1e-3);
        self.jumping = false;
        if self.air <= 1e-3 {
            self.top = self.pos.y;
        }
        self.hurt = HURT_S;
        self.heard_hurt = true;
        self.yaw = away.x.atan2(-away.z).rem_euclid(std::f32::consts::TAU);
        self.speed = RUN;
        self.turn_in = FLEE_S;
    }

    /// (v227) Ce que les autres joueurs en savent (`id` : sa place dans la foule).
    fn state(&self, id: u16) -> NpcState {
        NpcState {
            id,
            num: self.num,
            // (v318) Le paquet est en double précision (la partie compte depuis son origine).
            pos: self.pos.to_array().map(f64::from),
            yaw: self.yaw,
            speed: self.speed,
            push: (self.air > 0.0).then_some(self.push.to_array()),
            hurt: self.hurt > 0.0,
            hidden: self.hidden,
        }
    }

    /// (v227) PNJ d'une foule reçue, tel que le dit son état.
    fn received(world: &World, s: &NpcState) -> Npc {
        let pos = Vec3::from(s.pos.map(|v| v as f32));
        let mut n = Npc {
            name: format!("Pnj{}", s.num),
            pos,
            yaw: s.yaw,
            pitch: 0.0,
            speed: s.speed,
            turn_in: f32::INFINITY,
            cell: (pos.x.floor() as i32, pos.z.floor() as i32),
            walk: crate::session::Walk::new(pos),
            skin: None,
            covered: covered(world, pos),
            push: Vec3::ZERO,
            air: 0.0,
            hurt: 0.0,
            hidden: false,
            num: s.num,
            heard_hurt: false,
            hp: NPC_HP,
            top: pos.y,
            swim: false,
            jumping: false,
        };
        n.set(world, s);
        n
    }

    /// (v227) Nouvel état reçu du jeu qui mène la foule. Rend vrai s'il vient d'être frappé
    /// (son cri).
    fn set(&mut self, world: &World, s: &NpcState) -> bool {
        if s.num != self.num {
            self.num = s.num;
            self.name = format!("Pnj{}", s.num);
        }
        let pos = Vec3::from(s.pos.map(|v| v as f32));
        // Reparti d'ailleurs (réapparu, personnage caché qui se cache) : sa marche aussi.
        if pos.distance(self.pos) > 4.0 {
            self.walk = crate::session::Walk::new(pos);
        }
        self.pos = pos;
        let cell = (pos.x.floor() as i32, pos.z.floor() as i32);
        if cell != self.cell {
            self.cell = cell;
            self.covered = covered(world, pos);
        }
        self.yaw = s.yaw;
        self.speed = s.speed;
        match s.push {
            Some(v) => {
                self.push = Vec3::from(v);
                // (v238) Un saut (vers le haut, sans coup reçu) garde son élan contre la
                // marche, comme chez celui qui mène la foule.
                if self.air <= 0.0 {
                    self.jumping = v[1] > 0.0 && !s.hurt;
                }
                self.air = self.air.max(1e-3);
            }
            None => {
                self.push = Vec3::ZERO;
                self.air = 0.0;
                self.jumping = false;
            }
        }
        if s.hidden != self.hidden {
            self.hidden = s.hidden;
            self.skin = s.hidden.then(crate::bot_skin::hidden_skin);
        }
        let fresh = s.hurt && !self.heard_hurt;
        if fresh {
            self.hurt = HURT_S;
        }
        self.heard_hurt = s.hurt;
        fresh
    }
}

/// (v226) PNJ de `npcs` dont la boîte (0,6 × 1,8 × 0,6 bloc, comme un joueur) est touchée par
/// le rayon `eye` + t · `dir` à moins de `reach` blocs : le plus proche, et sa distance.
fn aimed_in(npcs: &[Npc], eye: Vec3, dir: Vec3, reach: f32) -> Option<(usize, f32)> {
    let h = crate::player::HALF_WIDTH;
    let mut best: Option<(usize, f32)> = None;
    for (i, n) in npcs.iter().enumerate() {
        // Loin du rayon : écarté sans calcul (des milliers par image, clic tenu).
        if (n.pos - eye).length_squared() > (reach + 2.0) * (reach + 2.0) {
            continue;
        }
        let min = n.pos - Vec3::new(h, 0.0, h);
        let max = n.pos + Vec3::new(h, crate::player::HEIGHT, h);
        if let Some(t) = ray_box(eye, dir, min, max) {
            if t <= reach && best.is_none_or(|b| t < b.1) {
                best = Some((i, t));
            }
        }
    }
    best
}

/// (v227) Ce que le jeu qui mène la foule en ligne doit encore en dire aux autres joueurs :
/// d'abord les PNJ qui ont changé, puis, à tour de rôle, tous les autres (en `REFRESH_S`
/// secondes : ceux qui arrivent les ont tous).
#[derive(Default)]
pub struct Outbox {
    queued: Vec<bool>,
    queue: Vec<u16>,
    cursor: usize,
    credit: f32,
    wait: f32,
    /// Le personnage caché vient d'être trouvé : par qui, où (part avec le paquet suivant).
    pub found: Option<(String, [f32; 3])>,
}

impl Outbox {
    pub fn new() -> Self {
        Self::default()
    }

    /// Une image : ce qui vient de changer dans la foule (`Crowd::changed`, vidé) ; toutes les
    /// `PACKET_EVERY` secondes, le paquet à envoyer (`center` : le joueur qui la mène).
    pub fn take(&mut self, crowd: &mut Crowd, center: Vec3, dt: f32) -> Option<Packet> {
        for i in crowd.changed.drain(..) {
            let k = i as usize;
            if k >= self.queued.len() {
                self.queued.resize(k + 1, false);
            }
            if !self.queued[k] {
                self.queued[k] = true;
                self.queue.push(i);
            }
        }
        let count = crowd.npcs.len();
        self.credit = (self.credit + count as f32 * dt / REFRESH_S).min(PACKET_MAX as f32);
        self.wait -= dt;
        if self.wait > 0.0 {
            return None;
        }
        // (Au rythme moyen voulu, sans rafale après une image lente.)
        self.wait = (self.wait + PACKET_EVERY).max(0.0);
        let mut ids: Vec<u16> = Vec::new();
        let first = self.queue.len().min(PACKET_MAX);
        for i in self.queue.drain(..first) {
            self.queued[i as usize] = false;
            if (i as usize) < count {
                ids.push(i);
            }
        }
        while self.credit >= 1.0 && ids.len() < PACKET_MAX && count > 0 {
            self.credit -= 1.0;
            let k = self.cursor % count;
            ids.push(k as u16);
            self.cursor = k + 1;
        }
        Some(Packet {
            center: center.to_array().map(f64::from),
            count: count as u16,
            npcs: ids.iter().map(|&i| crowd.npcs[i as usize].state(i)).collect(),
            found: self.found.take().map(|(w, a)| (w, a.map(f64::from))),
            stop: false,
        })
    }
}

/// (v227) Le dernier paquet d'une foule menée en ligne : elle s'arrête.
pub fn stop_packet(center: Vec3) -> Packet {
    Packet {
        center: center.to_array().map(f64::from),
        stop: true,
        ..Packet::default()
    }
}

/// (v227) Foule d'essai menée par le jeu d'un autre joueur (un OP), reçue par le relais : ses
/// PNJ avancent tout droit à la vitesse dite, sur le sol, jusqu'à la nouvelle suivante.
#[derive(Default)]
pub struct RemoteCrowd {
    /// PNJ dont on a reçu l'état (dans l'ordre d'arrivée).
    pub npcs: Vec<Npc>,
    /// Place dans `npcs` (plus un) de chaque PNJ de la foule, 0 : pas encore reçu.
    slot: Vec<u32>,
    /// Temps depuis le dernier paquet (s).
    pub heard: f32,
}

impl RemoteCrowd {
    /// Un paquet du jeu qui la mène. Rend où sont les PNJ qui viennent d'être frappés (leur
    /// cri).
    pub fn apply(&mut self, world: &World, p: &Packet) -> Vec<Vec3> {
        self.heard = 0.0;
        let count = p.count as usize;
        // Foule plus petite : ceux d'après n'y sont plus.
        if self.slot.len() > count {
            for id in count..self.slot.len() {
                if self.slot[id] != 0 {
                    self.forget(id);
                }
            }
        }
        self.slot.resize(count, 0);
        let mut hurt = Vec::new();
        for s in &p.npcs {
            let id = s.id as usize;
            if id >= count {
                continue;
            }
            match self.slot[id] {
                0 => {
                    let n = Npc::received(world, s);
                    if n.hurt > 0.0 {
                        hurt.push(n.pos);
                    }
                    self.npcs.push(n);
                    self.slot[id] = self.npcs.len() as u32;
                }
                k => {
                    let n = &mut self.npcs[k as usize - 1];
                    if n.set(world, s) {
                        hurt.push(n.pos);
                    }
                }
            }
        }
        hurt
    }

    /// Retire le PNJ `id` (sa place prise par le dernier de la liste).
    fn forget(&mut self, id: usize) {
        let k = self.slot[id] as usize - 1;
        self.slot[id] = 0;
        self.npcs.swap_remove(k);
        if k < self.npcs.len() {
            // Le dernier de la liste a pris sa place : sa place dans la foule le suit.
            let last = self.npcs.len() as u32 + 1;
            if let Some(s) = self.slot.iter_mut().find(|s| **s == last) {
                *s = k as u32 + 1;
            }
        }
    }

    /// Une image : chaque PNJ avance (bond, marche), sa marche est reprise.
    pub fn update(&mut self, world: &World, dt: f32) {
        self.heard += dt;
        for n in &mut self.npcs {
            n.hurt = (n.hurt - dt).max(0.0);
            if n.air > 0.0 {
                fly(world, n, dt);
            } else {
                glide(world, n, dt);
            }
            let (pos, yaw, pitch) = (n.pos, n.yaw, n.pitch);
            n.walk.update(pos, yaw, pitch, 0.0, n.air > 0.0, dt);
        }
    }

    /// PNJ visé à portée de coup, et sa distance (voir `Crowd::aimed`).
    pub fn aimed(&self, eye: Vec3, dir: Vec3, reach: f32) -> Option<(usize, f32)> {
        aimed_in(&self.npcs, eye, dir, reach)
    }

    /// Sa place dans la foule (celle que connaît le jeu qui la mène) du PNJ `i` de `npcs`.
    pub fn id_of(&self, i: usize) -> Option<u16> {
        self.slot
            .iter()
            .position(|&s| s as usize == i + 1)
            .map(|id| id as u16)
    }

    /// (v238) La foudre tombée en `at` (la même chez tous) : ses PNJ touchés sautent tout de
    /// suite, sans attendre le jeu qui mène la foule (lui leur ôte la vie, et dit la suite).
    /// Rend où ils étaient.
    /// (v247) `mine` : notre foudre ; le personnage caché qu'elle touche : son numéro, pour
    /// que le coup parte au jeu qui mène la foule (il l'a trouvé).
    pub fn struck(&mut self, at: Vec3, mine: bool) -> (Vec<Vec3>, Option<u16>) {
        let mut hit = Vec::new();
        let mut hidden = None;
        for i in 0..self.npcs.len() {
            let n = &mut self.npcs[i];
            if !crate::lightning::hurts_at(at, n.pos) {
                continue;
            }
            if n.hidden {
                if mine && hidden.is_none() {
                    hidden = Some(i);
                }
                continue;
            }
            hit.push(n.pos);
            n.knock(at);
        }
        (hit, hidden.and_then(|i| self.id_of(i)))
    }

    /// Coup donné ici au PNJ `i` : il recule tout de suite, sans attendre le jeu qui mène la
    /// foule (le personnage caché, lui, ne bouge que quand celui-ci l'a déplacé).
    pub fn hit(&mut self, i: usize, from: Vec3) {
        let n = &mut self.npcs[i];
        if !n.hidden {
            n.knock(from);
        }
    }
}

/// (v227) Un pas d'un PNJ d'une foule reçue : tout droit à sa vitesse, sur le sol ; devant un
/// mur ou un trou, il s'arrête (le jeu qui mène la foule le fait tourner : la nouvelle
/// arrive).
fn glide(world: &World, n: &mut Npc, dt: f32) {
    if n.speed <= 0.0 {
        return;
    }
    let (sy, cy) = n.yaw.sin_cos();
    let next = n.pos + Vec3::new(sy, 0.0, -cy) * n.speed * dt;
    let cell = (next.x.floor() as i32, next.z.floor() as i32);
    if cell == n.cell {
        n.pos = next;
        return;
    }
    match step_ground(world, next.x, next.z, n.pos.y) {
        Some(y) => {
            n.pos = Vec3::new(next.x, y, next.z);
            n.cell = cell;
            n.covered = covered(world, n.pos);
        }
        None => n.speed = 0.0,
    }
}

/// (v226) Un pas du bond d'un coup reçu : il recule (un mur l'arrête), monte, retombe sur le
/// sol (au plus 2 s en l'air : au-delà, posé sur le sol de sa colonne). Posé, il s'enfuit à la
/// vitesse décidée par le coup. (v227) Rend vrai à l'image où il se pose. (v238) Aussi un
/// saut ou une chute : rend alors la hauteur de la chute (0 dans l'eau, qui l'amortit).
fn fly(world: &World, n: &mut Npc, dt: f32) -> Option<f32> {
    n.air += dt;
    n.push.y -= GRAVITY * dt;
    n.top = n.top.max(n.pos.y);
    let mut next = n.pos + n.push * dt;
    let solid = |x: f32, y: f32, z: f32| {
        world
            .get(x.floor() as i32, y.floor() as i32, z.floor() as i32)
            .is_solid()
    };
    // Mur devant lui (le bord de sa boîte du côté où il recule, aux pieds, au milieu ou à la
    // tête) : plus de recul.
    let half = crate::player::HALF_WIDTH;
    let (ex, ez) = (
        next.x + half * n.push.x.signum() * f32::from(n.push.x != 0.0),
        next.z + half * n.push.z.signum() * f32::from(n.push.z != 0.0),
    );
    if [0.1, 0.9, 1.7]
        .iter()
        .any(|&h| solid(ex, n.pos.y + h, next.z) || solid(next.x, n.pos.y + h, ez))
    {
        next.x = n.pos.x;
        next.z = n.pos.z;
        // (v238) Sur un saut, il garde son élan : il passera une fois assez haut.
        if !n.jumping {
            n.push.x = 0.0;
            n.push.z = 0.0;
        }
    }
    // Plafond : il ne monte plus.
    if n.push.y > 0.0 && solid(next.x, next.y + 1.8, next.z) {
        next.y = n.pos.y;
        n.push.y = 0.0;
    }
    let mut land = None;
    // (v238) Dans l'eau : elle l'arrête (sans mal), il nage en surface.
    if n.push.y <= 0.0 {
        if let Some(top) = water_surface(world, next.x, next.y, next.z) {
            n.pos = Vec3::new(next.x, swim_feet(world, next.x, next.z, top), next.z);
            n.push = Vec3::ZERO;
            n.air = 0.0;
            n.jumping = false;
            n.swim = true;
            n.cell = (next.x.floor() as i32, next.z.floor() as i32);
            n.covered = covered(world, n.pos);
            return Some(0.0);
        }
    }
    if n.push.y <= 0.0 {
        // Sol : le haut du bloc plein sous ses pieds, s'il l'atteint à cette image.
        let floor = next.y.floor();
        if solid(next.x, floor - 0.5, next.z) && next.y <= floor + 0.001 + n.push.y.abs() * dt {
            land = Some(floor);
        } else if solid(next.x, next.y, next.z) {
            land = Some(next.y.floor() + 1.0);
        }
    }
    if land.is_none() && n.air > 2.0 {
        land = surface(world, next.x, next.z, next.y).or(Some(n.pos.y));
    }
    match land {
        Some(y) => {
            let fall = (n.top - y).max(0.0);
            n.pos = Vec3::new(next.x, y, next.z);
            n.push = Vec3::ZERO;
            n.air = 0.0;
            n.jumping = false;
            n.swim = false;
            n.top = y;
            n.cell = (next.x.floor() as i32, next.z.floor() as i32);
            n.covered = covered(world, n.pos);
            Some(fall)
        }
        None => {
            n.pos = next;
            None
        }
    }
}

/// (v238) Ce qu'un PNJ trouve en entrant dans la colonne (`x`, `z`), ses pieds à `feet_y`.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Ahead {
    /// Un sol à cette hauteur (le même, une marche plus bas, ou une berge en sortant de
    /// l'eau).
    Walk(f32),
    /// Un bloc devant ses pieds, la place au-dessus : il saute.
    Jump,
    /// Rien sous ses pieds : il tombe.
    Fall,
    /// De l'eau : il nage, les pieds à cette hauteur.
    Swim(f32),
    /// Un mur (deux blocs, ou un plafond trop bas) : demi-tour.
    Wall,
}

fn ahead(world: &World, from: Vec3, x: f32, z: f32, swimming: bool) -> Ahead {
    use minelib_world::registry::{fluid, Fluid};
    let (bx, bz) = (x.floor() as i32, z.floor() as i32);
    // Un chunk pas chargé : comme un mur (il ne s'y jette pas dans le vide).
    if !world.contains(minelib_world::ChunkPos::containing(bx, bz)) {
        return Ahead::Wall;
    }
    let feet_y = from.y;
    let y0 = feet_y.floor() as i32;
    let solid = |y: i32| world.get(bx, y, bz).is_solid();
    // De la lave devant lui ou sous ses pieds : il n'y va pas.
    let lava = |y: i32| matches!(fluid(world.get(bx, y, bz)), Some((Fluid::Lava, _)));
    if lava(y0) || lava(y0 - 1) {
        return Ahead::Wall;
    }
    if swimming {
        // En nageant : de l'eau encore, ou une berge à moins d'un bloc au-dessus de la
        // surface (il s'y hisse), sinon un mur.
        let surface = feet_y + SWIM_DEPTH;
        if let Some(top) = water_surface(world, x, feet_y + 0.5, z) {
            return Ahead::Swim(swim_feet(world, x, z, top));
        }
        let s0 = surface.floor() as i32;
        return match (s0 - 1..=s0 + 1)
            .rev()
            .find(|&y| solid(y) && !solid(y + 1) && !solid(y + 2))
        {
            Some(y) => Ahead::Walk((y + 1) as f32),
            None => Ahead::Wall,
        };
    }
    if solid(y0) {
        // Une marche : la place au-dessus d'elle, et au-dessus de sa tête pour sauter.
        let head = world
            .get(from.x.floor() as i32, y0 + 2, from.z.floor() as i32)
            .is_solid();
        return if !solid(y0 + 1) && !solid(y0 + 2) && !head {
            Ahead::Jump
        } else {
            Ahead::Wall
        };
    }
    if solid(y0 + 1) {
        return Ahead::Wall;
    }
    if let Some(top) = water_surface(world, x, feet_y - 0.5, z) {
        return Ahead::Swim(swim_feet(world, x, z, top));
    }
    if solid(y0 - 1) {
        Ahead::Walk(y0 as f32)
    } else {
        Ahead::Fall
    }
}

/// (v238) Pieds d'un PNJ dans l'eau dont la surface est à `top` (colonne `x`, `z`) : à
/// `SWIM_DEPTH` sous la surface, ou sur le fond si l'eau est moins profonde (il y marche).
fn swim_feet(world: &World, x: f32, z: f32, top: f32) -> f32 {
    let (bx, bz) = (x.floor() as i32, z.floor() as i32);
    let want = top - SWIM_DEPTH;
    let mut y = top.floor() as i32;
    while (y + 1) as f32 > want {
        if world.get(bx, y, bz).is_solid() {
            return (y + 1) as f32;
        }
        y -= 1;
    }
    want
}

/// (v238) Surface de l'eau de la colonne (`x`, `z`) si le point (`x`, `y`, `z`) est dans
/// l'eau : le haut du dernier bloc d'eau au-dessus (sa hauteur de fluide comprise).
fn water_surface(world: &World, x: f32, y: f32, z: f32) -> Option<f32> {
    use minelib_world::registry::{fluid, fluid_height, Fluid};
    // (Le haut d'une colonne d'eau : sa propre hauteur de fluide.)
    let (bx, bz) = (x.floor() as i32, z.floor() as i32);
    let mut yy = y.floor() as i32;
    let is_water = |yy: i32| matches!(fluid(world.get(bx, yy, bz)), Some((Fluid::Water, _)));
    if !is_water(yy) {
        return None;
    }
    while is_water(yy + 1) && yy < y.floor() as i32 + 64 {
        yy += 1;
    }
    let h = fluid_height(world.get(bx, yy, bz), world.get(bx, yy + 1, bz)).map_or(0.9, |f| f.1);
    Some(yy as f32 + h)
}

/// (v226) Distance du rayon `o` + t · `d` à la boîte (`min`, `max`), s'il la touche.
fn ray_box(o: Vec3, d: Vec3, min: Vec3, max: Vec3) -> Option<f32> {
    let inv = d.recip();
    let (a, b) = ((min - o) * inv, (max - o) * inv);
    let near = a.min(b).max_element();
    let far = a.max(b).min_element();
    (far >= near.max(0.0)).then_some(near.max(0.0))
}

/// Le ciel est-il bouché au-dessus de ses yeux ? (La même lumière que pour les joueurs.)
fn covered(world: &World, pos: Vec3) -> bool {
    crate::session::covered_above(world, crate::session::eye_of(pos, 0.0))
}

/// Sol de la colonne (`x`, `z`) le plus haut entre 40 blocs sous `near_y` et 40 au-dessus :
/// un bloc plein avec deux cases libres au-dessus. `None` : rien (chunk absent, vide).
fn surface(world: &World, x: f32, z: f32, near_y: f32) -> Option<f32> {
    let (bx, bz) = (x.floor() as i32, z.floor() as i32);
    // (v234) Chunk pas chargé : rien à lire (81 hauteurs d'air).
    if !world.contains(minelib_world::ChunkPos::containing(bx, bz)) {
        return None;
    }
    let y0 = near_y.floor() as i32;
    ((y0 - 40)..=(y0 + 40))
        .rev()
        .find(|&y| {
            world.get(bx, y, bz).is_solid()
                && !world.get(bx, y + 1, bz).is_solid()
                && !world.get(bx, y + 2, bz).is_solid()
        })
        .map(|y| (y + 1) as f32)
}

/// Sol de la case où l'on arrive, au plus une marche plus haut ou trois plus bas que les
/// pieds `feet_y` (on ne grimpe pas aux murs, on ne saute pas dans un trou).
fn step_ground(world: &World, x: f32, z: f32, feet_y: f32) -> Option<f32> {
    let (bx, bz) = (x.floor() as i32, z.floor() as i32);
    let y0 = feet_y.floor() as i32;
    ((y0 - 4)..=y0)
        .rev()
        .find(|&y| {
            world.get(bx, y, bz).is_solid()
                && !world.get(bx, y + 1, bz).is_solid()
                && !world.get(bx, y + 2, bz).is_solid()
        })
        .map(|y| (y + 1) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use minelib_world::{BlockId, ChunkPos};

    /// Un sol plat de 32 × 32 chunks (sol en y = 10, pieds en y = 11).
    fn flat() -> World {
        let mut w = World::new(0);
        for cz in -16..16 {
            for cx in -16..16 {
                w.insert_empty(ChunkPos { x: cx, z: cz });
            }
        }
        // (v238) Sur tous les chunks : un PNJ qui sortirait du sol tomberait dans le vide.
        for z in -256..256 {
            for x in -256..256 {
                w.set(x, 10, z, BlockId::STONE);
            }
        }
        w
    }

    /// Une foule d'un seul PNJ, immobile, en `at`.
    fn one_at(w: &World, at: Vec3) -> Crowd {
        let mut c = Crowd::new();
        let mut n = c.spawn(w, at).unwrap();
        n.pos = at;
        n.cell = (at.x.floor() as i32, at.z.floor() as i32);
        n.speed = 0.0;
        n.turn_in = 100.0;
        c.npcs.push(n);
        c.changed.push(0);
        c
    }

    /// Une foule qui ne grandit plus : ce PNJ seul.
    fn alone(w: &World, at: Vec3) -> Crowd {
        let mut c = one_at(w, at);
        c.t = f32::NEG_INFINITY;
        c
    }

    /// Un PNJ seul qui marche droit vers +x (sans tourner).
    fn walker(w: &World, at: Vec3) -> Crowd {
        let mut c = alone(w, at);
        c.npcs[0].yaw = std::f32::consts::FRAC_PI_2;
        c.npcs[0].speed = 4.3;
        c
    }

    fn run(c: &mut Crowd, w: &World, seconds: f32) {
        let skins = crate::skin::Skins::default();
        for _ in 0..(seconds * 60.0) as usize {
            c.update(w, &skins, Vec3::new(0.5, 11.0, 0.5), 1.0 / 60.0);
        }
    }

    /// (v344) La foule d'essai de 4 000 (26 novembre, 02:54) : chaque joueur simulé voyait son
    /// skin demandé au relais, une demande qui attend sa réponse sur la connexion du joueur ;
    /// 4 000 d'affilée la bloquaient des minutes (« le relais ne répond pas », puis des
    /// réponses décalées : un /tp recevait le skin de pnj4002). Un joueur simulé n'a pas de
    /// compte : rien n'est plus demandé pour lui.
    #[test]
    fn simulated_players_never_ask_the_relay_for_skins() {
        let w = flat();
        let mut c = Crowd::new();
        let mut skins = crate::skin::Skins::default();
        for _ in 0..(40.0 * 60.0) as usize {
            c.update(&w, &skins, Vec3::new(0.5, 11.0, 0.5), 1.0 / 60.0);
        }
        assert!(c.npcs.len() > 1000, "{} joueurs simulés", c.npcs.len());
        assert!(skins.take_wanted_max(usize::MAX).is_empty());
        // Un vrai joueur, lui, est demandé (par paquets : quelques-uns à la fois).
        for p in ["Haumax", "Viendax", "Zoe"] {
            assert!(skins.of(p).is_none());
        }
        assert_eq!(skins.take_wanted_max(2), vec!["haumax", "viendax"]);
        assert_eq!(skins.take_wanted_max(2), vec!["zoe"]);
        assert!(skins.take_wanted_max(2).is_empty());
    }

    /// (v238) « Il a la même vie qu'un joueur » : vingt coups pour le tuer ; tué, il
    /// réapparaît ailleurs (loin de qui l'a tué), avec toute sa vie et le même pseudo.
    #[test]
    fn a_killed_npc_comes_back_elsewhere_with_full_life() {
        let w = flat();
        let mut c = alone(&w, Vec3::new(3.5, 11.0, 0.5));
        let name = c.npcs[0].name.clone();
        for k in 1..=20 {
            let eye = c.npcs[0].pos - Vec3::X * 1.5 + Vec3::Y * 1.62;
            let hit = c.hit(&w, 0, eye, Vec3::new(0.5, 11.0, 0.5));
            if k < 20 {
                assert_eq!(hit, Hit::Npc, "coup {k}");
                assert_eq!(c.npcs[0].hp, NPC_HP - k as f32);
                // Il retombe avant le coup suivant.
                run(&mut c, &w, 0.5);
            } else {
                assert_eq!(hit, Hit::Killed, "le vingtième coup le tue");
                let n = &c.npcs[0];
                assert_eq!(n.hp, NPC_HP, "toute sa vie");
                assert_eq!(n.name, name, "le même pseudo");
                assert!(n.pos.distance(eye) >= RESPAWN_AWAY, "ailleurs : {}", n.pos);
            }
        }
        // La foudre : cinq points par coup (elle le fait sauter), la quatrième le tue.
        for k in 1..=4 {
            let at = c.npcs[0].pos;
            let strike = at + Vec3::X;
            let hit = c.struck(&w, strike, Vec3::new(0.5, 11.0, 0.5), false);
            assert_eq!(hit, (vec![at], false), "foudre {k}");
            let n = &c.npcs[0];
            if k < 4 {
                assert_eq!(n.hp, NPC_HP - 5.0 * k as f32, "foudre {k}");
                assert!(n.air > 0.0, "il saute");
            } else {
                assert_eq!(n.hp, NPC_HP, "réapparu avec toute sa vie");
                assert!(n.pos.distance(strike) >= RESPAWN_AWAY, "ailleurs : {}", n.pos);
            }
            run(&mut c, &w, 0.5);
        }
        // Trop loin d'elle : rien.
        let at = c.npcs[0].pos;
        assert!(c.struck(&w, at + Vec3::X * 4.0, Vec3::new(0.5, 11.0, 0.5), true).0.is_empty());
        assert_eq!(c.npcs[0].hp, NPC_HP);
        // (v247) Notre foudre sur le personnage caché : trouvé (il part se cacher ailleurs) ;
        // celle d'un autre, ou la foudre de l'orage : rien.
        let hidden = c.npcs.len();
        let mut h = one_at(&w, Vec3::new(3.5, 11.0, 0.5));
        let mut n = h.npcs.pop().unwrap();
        n.hidden = true;
        c.npcs.push(n);
        let at = c.npcs[hidden].pos;
        assert!(!c.struck(&w, at + Vec3::X, Vec3::new(0.5, 11.0, 0.5), false).1);
        assert_eq!(c.npcs[hidden].pos, at, "pas trouvé par la foudre de l'orage");
        let (_, found) = c.struck(&w, at + Vec3::X, Vec3::new(0.5, 11.0, 0.5), true);
        assert!(found, "trouvé par notre foudre");
        assert!(c.npcs[hidden].hidden);
    }

    /// (v238) « Les mêmes déplacements possibles » : il saute sur une marche d'un bloc, tombe
    /// d'un rebord (et se fait mal au-delà de 4 blocs, comme un joueur), nage en surface
    /// dans l'eau et en ressort sur la berge ; un mur de deux blocs le fait tourner.
    #[test]
    fn npcs_jump_fall_and_swim_like_players() {
        // Une estrade d'un bloc de haut à partir de x = 6 (sous un plafond, au besoin).
        let stage = |ceiling: bool| {
            let mut w = flat();
            for z in -3..4 {
                for x in 6..20 {
                    w.set(x, 11, z, BlockId::STONE);
                }
                for x in 0..6 {
                    if ceiling {
                        w.set(x, 13, z, BlockId::STONE);
                    }
                }
            }
            w
        };
        let w = stage(false);
        let mut c = walker(&w, Vec3::new(3.5, 11.0, 0.5));
        run(&mut c, &w, 1.5);
        let n = &c.npcs[0];
        assert!(n.pos.x > 7.0 && n.pos.y == 12.0, "monté sur l'estrade : {}", n.pos);
        // Sous un plafond trop bas pour sauter : demi-tour.
        let w = stage(true);
        let mut c = walker(&w, Vec3::new(3.5, 11.0, 0.5));
        run(&mut c, &w, 1.5);
        let n = &c.npcs[0];
        assert!(n.pos.x < 6.0 && n.pos.y == 11.0, "pas de saut sous le plafond : {}", n.pos);
        // Un rebord de 6 blocs : il tombe, perd 2 points, et repart.
        let mut w = flat();
        for x in 0..5 {
            for z in -3..4 {
                for y in 11..17 {
                    w.set(x, y, z, BlockId::STONE);
                }
            }
        }
        let mut c = walker(&w, Vec3::new(2.5, 17.0, 0.5));
        run(&mut c, &w, 2.0);
        let n = &c.npcs[0];
        assert_eq!(n.pos.y, 11.0, "tombé en bas : {}", n.pos);
        assert_eq!(n.hp, NPC_HP - 2.0, "une chute de 6 blocs ôte 2 points");
        // Une piscine de deux blocs (eau de x = 5 à 9) : il y nage, en surface, puis remonte
        // sur la berge.
        let mut w = flat();
        for x in 5..10 {
            for z in -3..4 {
                w.set(x, 8, z, BlockId::STONE);
                w.set(x, 9, z, BlockId::WATER);
                w.set(x, 10, z, BlockId::WATER);
            }
        }
        let mut c = walker(&w, Vec3::new(3.5, 11.0, 0.5));
        let mut swam = false;
        let skins = crate::skin::Skins::default();
        for _ in 0..(6.0 * 60.0) as usize {
            c.update(&w, &skins, Vec3::new(0.5, 11.0, 0.5), 1.0 / 60.0);
            let n = &c.npcs[0];
            if n.swim {
                swam = true;
                assert!(n.pos.y > 9.3 && n.pos.y < 10.0, "en surface : {}", n.pos);
            }
        }
        let n = &c.npcs[0];
        assert!(swam, "il a nagé");
        assert!(n.pos.x > 10.0 && n.pos.y == 11.0 && !n.swim, "sur la berge : {}", n.pos);
        assert_eq!(n.hp, NPC_HP, "l'eau ne fait pas mal");
        // Un mur de deux blocs : demi-tour.
        let mut w = flat();
        for z in -3..4 {
            w.set(6, 11, z, BlockId::STONE);
            w.set(6, 12, z, BlockId::STONE);
        }
        let mut c = walker(&w, Vec3::new(3.5, 11.0, 0.5));
        run(&mut c, &w, 1.5);
        let n = &c.npcs[0];
        assert!(n.pos.x < 6.0 && n.pos.y == 11.0, "arrêté par le mur : {}", n.pos);
    }

    /// (v234) Loin de la foule (en vol, plus de sol à moins de 40 blocs, chunks pas chargés),
    /// les PNJ restés loin ne cherchent pas tous un sol à chaque image : 64 au plus. Avant,
    /// les 4 000 le faisaient (« logique et physique » à 11 ms, 77 FPS).
    #[test]
    fn far_from_the_crowd_the_respawns_are_bounded_per_frame() {
        let w = flat();
        let skins = crate::skin::Skins::default();
        let mut c = Crowd::new();
        let center = Vec3::new(0.0, 11.0, 0.0);
        while c.npcs.len() < MAX {
            if let Some(n) = c.spawn(&w, center) {
                c.npcs.push(n);
            }
        }
        c.t = 1000.0;
        c.hidden_in = true;
        for away in [Vec3::new(5000.0, 300.0, 0.0), Vec3::new(0.0, 300.0, 600.0)] {
            c.tries = 0;
            c.update(&w, &skins, away, 0.016);
            assert!(
                c.tries <= RESPAWNS_PER_FRAME,
                "{} recherches de sol en une image",
                c.tries
            );
        }
        // Revenu au sol, loin d'eux : ils reviennent, 64 par image au plus.
        for n in &mut c.npcs {
            n.pos = Vec3::new(190.0, 11.0, 190.0);
            n.cell = (190, 190);
        }
        let back = Vec3::new(-100.0, 11.0, -100.0);
        c.update(&w, &skins, back, 0.016);
        let near = c
            .npcs
            .iter()
            .filter(|n| (n.pos - back).length() <= GONE)
            .count();
        assert!(near > 0 && near <= RESPAWNS_PER_FRAME, "{near} revenus");
    }

    /// (v234) « Hauteur gagnée en étant tapé / 3 » : frappé quatre fois par seconde pendant
    /// trois secondes (de tout près, où qu'il soit), un PNJ ne monte plus dans le ciel (avant :
    /// plus de 5 blocs) ; il ne quitte presque pas le sol.
    #[test]
    fn hits_in_a_row_no_longer_lift_an_npc_into_the_sky() {
        let w = flat();
        let skins = crate::skin::Skins::default();
        let mut c = one_at(&w, Vec3::new(3.5, 11.0, 0.5));
        let mut top = 0.0f32;
        for k in 0..180 {
            if k % 15 == 0 {
                let p = c.npcs[0].pos;
                let eye = p - Vec3::X * 1.5 + Vec3::Y * 1.62;
                c.hit(&w, 0, eye, eye);
            }
            c.update(&w, &skins, Vec3::new(0.5, 12.62, 0.5), 1.0 / 60.0);
            top = top.max(c.npcs[0].pos.y - 11.0);
        }
        assert!(top < 0.3, "monté de {top} bloc");
    }

    /// (v226) Frappé, un PNJ recule d'un bond, rougit, retombe sur le sol, puis s'enfuit en
    /// courant loin du coup ; un mur arrête son recul. Clic dans le vide ou trop loin : rien.
    #[test]
    fn a_hit_npc_jumps_back_and_runs_away() {
        let w = flat();
        let skins = crate::skin::Skins::default();
        let eye = Vec3::new(0.5, 12.62, 0.5);
        let mut c = one_at(&w, Vec3::new(3.5, 11.0, 0.5));
        assert!(c.aimed(eye, Vec3::Z, 4.5).is_none(), "à côté");
        assert!(c.aimed(eye, Vec3::X, 2.5).is_none(), "trop loin");
        let (i, t) = c.aimed(eye, Vec3::X, 4.5).unwrap();
        assert!((t - 2.7).abs() < 1e-4, "{t}");
        assert_eq!(c.hit(&w, i, eye, eye), Hit::Npc);
        assert!(c.npcs[0].hurt > 0.0);
        let mut top = 0.0f32;
        for _ in 0..12 {
            c.update(&w, &skins, eye, 1.0 / 60.0);
            top = top.max(c.npcs[0].pos.y);
        }
        // (v234) Un sixième de bloc de haut (un demi avant). (v247) Un douzième.
        assert!(top > 11.04 && top < 11.14, "bond : {top}");
        for _ in 0..30 {
            c.update(&w, &skins, eye, 1.0 / 60.0);
        }
        let n = &c.npcs[0];
        assert_eq!(n.pos.y, 11.0, "retombé sur le sol");
        assert!(n.pos.x > 4.5, "reculé : {}", n.pos.x);
        assert_eq!(n.hurt, 0.0, "plus rouge");
        let x = n.pos.x;
        for _ in 0..60 {
            c.update(&w, &skins, eye, 1.0 / 60.0);
        }
        assert!(c.npcs[0].pos.x > x + 4.0, "s'enfuit : {}", c.npcs[0].pos.x);
        assert!(c.npcs[0].pos.z.abs() < 1.0, "droit devant lui");

        // Un mur juste derrière lui : il n'y entre pas.
        let mut w = flat();
        for y in 11..14 {
            for z in -3..4 {
                w.set(4, y, z, BlockId::STONE);
            }
        }
        let mut c = one_at(&w, Vec3::new(3.5, 11.0, 0.5));
        let (i, _) = c.aimed(eye, Vec3::X, 4.5).unwrap();
        c.hit(&w, i, eye, eye);
        for _ in 0..120 {
            c.update(&w, &skins, eye, 1.0 / 60.0);
            let n = &c.npcs[0];
            // En l'air, sa boîte s'arrête au mur ; puis, comme quand il marche, il fait
            // demi-tour en y arrivant.
            let wall = if n.air > 0.0 { 4.0 - crate::player::HALF_WIDTH } else { 4.0 };
            assert!(n.pos.x <= wall + 1e-4, "dans le mur : {}", n.pos.x);
        }
        assert_eq!(c.npcs[0].pos.y, 11.0);
    }

    /// (v226) Un seul personnage caché dans la foule, loin du joueur, avec son skin ;
    /// trouvé (frappé), il repart se cacher loin de qui l'a trouvé, sous un autre pseudo.
    #[test]
    fn the_hidden_character_hides_again_when_found() {
        let w = flat();
        let skins = crate::skin::Skins::default();
        let center = Vec3::new(0.0, 11.0, 0.0);
        let mut c = Crowd::new();
        for _ in 0..(20.0 / 0.05) as usize {
            c.update(&w, &skins, center, 0.05);
        }
        let hidden: Vec<usize> = (0..c.npcs.len()).filter(|&i| c.npcs[i].hidden).collect();
        assert_eq!(hidden.len(), 1);
        let i = hidden[0];
        let skin = crate::bot_skin::hidden_skin();
        assert!(c.npcs[i]
            .skin
            .as_ref()
            .is_some_and(|s| std::sync::Arc::ptr_eq(s, &skin)));
        // Il se cache (et marche) dans la foule ; son pseudo est un pseudo de la foule.
        assert!(c.npcs[i].name.starts_with("Pnj"));
        // Trouvé : ailleurs, loin de qui l'a trouvé, sous un autre pseudo, toujours seul.
        let (name, at) = (c.npcs[i].name.clone(), c.npcs[i].pos);
        let finder = at + Vec3::new(-2.0, 1.62, 0.0);
        assert_eq!(c.hit(&w, i, finder, center), Hit::Hidden);
        let n = &c.npcs[i];
        assert!(n.hidden);
        assert_ne!(n.name, name);
        assert!(n.pos.distance(finder) >= HIDE_MIN, "{}", n.pos.distance(finder));
        assert_eq!(n.pos.y, 11.0);
        assert_eq!(c.npcs.iter().filter(|n| n.hidden).count(), 1);
        // Les skins repris de temps en temps ne lui enlèvent pas le sien.
        for _ in 0..200 {
            c.update(&w, &skins, center, 0.05);
        }
        let n = c.npcs.iter().find(|n| n.hidden).unwrap();
        assert!(n.skin.as_ref().is_some_and(|s| std::sync::Arc::ptr_eq(s, &skin)));
    }

    /// (v227) Une foule menée en ligne, reçue par un autre joueur : chaque PNJ y est, au
    /// même endroit à un pas près (les paquets passent par leurs octets), Pistache compris ;
    /// chacun est redit au moins toutes les `REFRESH_S` secondes ; arrêtée, le dernier paquet
    /// le dit.
    #[test]
    fn a_crowd_led_online_is_seen_the_same_by_others() {
        let w = flat();
        let skins = crate::skin::Skins::default();
        let center = Vec3::new(0.0, 11.0, 0.0);
        let mut c = Crowd::new();
        let mut out = Outbox::new();
        let mut far = RemoteCrowd::default();
        let mut late = RemoteCrowd::default();
        let (mut sent, mut packets, mut biggest) = (0usize, 0usize, 0usize);
        let dt = 1.0 / 60.0;
        let steps = (40.0 / dt) as usize;
        for step in 0..steps {
            c.update(&w, &skins, center, dt);
            if let Some(p) = out.take(&mut c, center, dt) {
                let data = minelib_net::crowd::pack(&p);
                biggest = biggest.max(data.len());
                sent += data.len();
                packets += 1;
                let p = minelib_net::crowd::unpack(&data).unwrap();
                far.apply(&w, &p);
                // Un autre arrive au bout de 30 s : en `REFRESH_S`, il a tout le monde.
                if step as f32 * dt >= 30.0 {
                    late.apply(&w, &p);
                }
            }
            far.update(&w, dt);
            if step as f32 * dt >= 30.0 {
                late.update(&w, dt);
            }
        }
        // Le paquet suivant : ce qui a changé aux toutes dernières images.
        let p = out.take(&mut c, center, PACKET_EVERY).unwrap();
        let p = minelib_net::crowd::unpack(&minelib_net::crowd::pack(&p)).unwrap();
        far.apply(&w, &p);
        late.apply(&w, &p);
        // Un paquet toutes les 50 ms ; une quinzaine d'octets par PNJ redit.
        assert!((packets as f32 - 40.0 / PACKET_EVERY).abs() < 4.0, "{packets} paquets");
        assert!(biggest < 24 * 1024, "{biggest} octets");
        let per_s = sent as f32 / 40.0;
        eprintln!(
            "{packets} paquets, {per_s:.0} octets/s pour {} PNJ, le plus gros : {biggest} octets",
            c.npcs.len()
        );
        assert!(per_s < 40_000.0, "{per_s} octets/s pour {} PNJ", c.npcs.len());
        for rc in [&far, &late] {
            assert_eq!(rc.npcs.len(), c.npcs.len());
            for (id, n) in c.npcs.iter().enumerate() {
                let k = rc.slot[id] as usize;
                assert!(k > 0, "PNJ {id} pas reçu");
                let m = &rc.npcs[k - 1];
                assert_eq!((&m.name, m.hidden), (&n.name, n.hidden));
                let d = m.pos.distance(n.pos);
                assert!(d < 0.6, "{} : {d} blocs d'écart", n.name);
            }
        }
        let i = c.npcs.iter().position(|n| n.hidden).unwrap();
        let k = far.slot[i] as usize - 1;
        assert!(far.npcs[k]
            .skin
            .as_ref()
            .is_some_and(|s| std::sync::Arc::ptr_eq(s, &crate::bot_skin::hidden_skin())));
        // Arrêtée : le dernier paquet le dit.
        let p = stop_packet(center);
        let q = minelib_net::crowd::unpack(&minelib_net::crowd::pack(&p)).unwrap();
        assert!(q.stop && q.npcs.is_empty());
    }

    /// (v227) Un PNJ de la foule d'un OP frappé par un autre joueur : il recule tout de suite
    /// chez celui qui frappe, puis chez l'OP (le coup lui arrive) et chez un troisième, qui
    /// l'entend crier, une seule fois ; celui qui a frappé ne l'entend pas une deuxième fois.
    #[test]
    fn a_hit_on_someone_elses_crowd_is_seen_by_everyone_once() {
        let w = flat();
        let skins = crate::skin::Skins::default();
        let center = Vec3::new(0.5, 11.0, 0.5);
        let mut c = one_at(&w, Vec3::new(3.5, 11.0, 0.5));
        let mut out = Outbox::new();
        let mut rcs = [RemoteCrowd::default(), RemoteCrowd::default()];
        // Quelques images de l'OP et des deux autres ; les cris entendus par chacun.
        let run = |c: &mut Crowd, out: &mut Outbox, rcs: &mut [RemoteCrowd; 2], frames| {
            let dt = 1.0 / 60.0;
            let mut cries = [0, 0];
            for _ in 0..frames {
                c.update(&w, &skins, center, dt);
                if let Some(p) = out.take(c, center, dt) {
                    let p = minelib_net::crowd::unpack(&minelib_net::crowd::pack(&p)).unwrap();
                    for (k, rc) in rcs.iter_mut().enumerate() {
                        cries[k] += rc.apply(&w, &p).len();
                    }
                }
                for rc in rcs.iter_mut() {
                    rc.update(&w, dt);
                }
            }
            cries
        };
        assert_eq!(run(&mut c, &mut out, &mut rcs, 30), [0, 0]);
        let hitter = &mut rcs[0];
        // Il frappe le PNJ chez lui : il recule tout de suite.
        let eye = Vec3::new(0.5, 12.62, 0.5);
        let (i, _) = hitter.aimed(eye, Vec3::X, 4.5).unwrap();
        let id = hitter.id_of(i).unwrap();
        assert_eq!(id, 0);
        hitter.hit(i, eye);
        assert!(hitter.npcs[i].air > 0.0 && hitter.npcs[i].hurt > 0.0);
        // Le coup arrive à l'OP (un peu plus tard), qui le dit à tous.
        assert_eq!(c.hit(&w, id as usize, eye, center), Hit::Npc);
        let cries = run(&mut c, &mut out, &mut rcs, 90);
        assert_eq!(cries, [0, 1], "cris entendus");
        for rc in &rcs {
            let n = &rc.npcs[0];
            assert_eq!(n.pos.y, 11.0, "retombé");
            assert!(n.pos.x > 5.0, "reculé puis enfui : {}", n.pos.x);
            assert!(n.pos.distance(c.npcs[0].pos) < 0.6);
        }
    }

    #[test]
    fn the_crowd_grows_to_four_thousand_in_two_minutes_and_walks_on_the_ground() {
        assert_eq!(Crowd::wanted(0.0), 0);
        assert_eq!(Crowd::wanted(60.0), 2000);
        assert_eq!(Crowd::wanted(120.0), MAX);
        assert_eq!(Crowd::wanted(500.0), MAX);
        let w = flat();
        let mut c = Crowd::new();
        let skins = crate::skin::Skins::default();
        let center = Vec3::new(0.0, 11.0, 0.0);
        for _ in 0..(30.0 / 0.05) as usize {
            c.update(&w, &skins, center, 0.05);
        }
        assert!(
            (Crowd::wanted(30.0) as i64 - c.npcs.len() as i64).abs() <= 64,
            "{} joueurs après 30 s",
            c.npcs.len()
        );
        for n in &c.npcs {
            assert_eq!(n.pos.y, 11.0, "{} : sur le sol", n.name);
            assert!((n.pos - center).length() <= GONE + 1.0);
        }
        // Des pseudos tous différents.
        let names: std::collections::HashSet<&str> =
            c.npcs.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names.len(), c.npcs.len());
    }
}
