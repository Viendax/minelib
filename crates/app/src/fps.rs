//! Mode FPS : 100 points de vie, armes (achat, tir, recul), balles traçantes et impacts.
//!
//! - **Achat** (touche B par défaut) : toutes les armes, avec leur prix. En solo il n'y a pas
//!   d'argent : le solde reste à 0 $ et les achats sont gratuits.
//! - **Tir** : clic gauche (maintenu pour les armes automatiques), R recharge, 1 / 2 arme
//!   principale / pistolet, clic droit : lunette des fusils qui en ont une.
//! - **Recul** : chaque arme suit un motif fixe (montée puis balayage gauche / droite), qui
//!   se retrouve à l'écran (la vue remonte) et dans la direction des balles ; il se remet à
//!   zéro quand on arrête de tirer. Imprécision plus forte en courant et en l'air.
//! - **Balles** : traçante lumineuse du canon jusqu'au point touché, étincelles à l'impact,
//!   impact noir sur le bloc (les 64 derniers).

use glam::{Mat4, Vec3};
use minelib_platform::Input;
use minelib_render::{Ui, VmVertex, NO_TEXTURE};
use minelib_world::raycast::raycast;
use minelib_world::World;

use crate::audio::Sfx;
use crate::equip::{self, Effect, Nade, Thrown};
use crate::weapons::{self, Category, WeaponDef, WEAPONS};

/// Facteur appliqué aux dégâts de chute en mode FPS.
const FALL_SCALE: f32 = 0.25;
pub const MAX_HP: u32 = 100;
const RANGE: f32 = 250.0;
const BULLET_SPEED: f32 = 300.0;
const TRACER_LEN: f32 = 3.0;
const MAX_HOLES: usize = 64;
/// Champ de vision de la main et de l'arme (comme la main du mode bloc).
const VIEW_FOV: f32 = 70.0;

#[derive(Clone, Copy)]
pub struct Gun {
    pub def: usize,
    pub mag: u32,
    pub reserve: u32,
}

impl Gun {
    fn new(def: usize) -> Self {
        let d = &WEAPONS[def];
        Self {
            def,
            mag: d.magazine,
            reserve: d.reserve,
        }
    }
}

struct Tracer {
    from: Vec3,
    dir: Vec3,
    dist: f32,
    travelled: f32,
}

/// Entrées de tir pour une image.
#[derive(Default, Clone, Copy)]
pub struct Controls {
    pub trigger_pressed: bool,
    pub trigger_held: bool,
    /// Bouton de tir enfoncé, même hors jeu (menu ouvert, souris libre) : sert à attendre
    /// qu'il soit vraiment relâché après un clic de menu.
    pub trigger_down: bool,
    pub reload: bool,
    pub scope: bool,
    pub slot: Option<usize>,
    /// Arme précédente (Q), arme suivante (bascule principale / secondaire).
    pub last_weapon: bool,
    pub next_weapon: bool,
    /// Lâcher l'arme principale.
    pub drop: bool,
    /// Racheter la dernière arme achetée.
    pub rebuy: bool,
    /// Vitesse horizontale (m/s) et au sol ou non.
    pub speed: f32,
    pub on_ground: bool,
    /// Vitesse du joueur (élan donné aux grenades).
    pub vel: Vec3,
    /// Molette : +1 arme suivante, −1 précédente (couteau, pistolet, principale, grenades).
    pub wheel: i32,
    /// Touche d'une grenade précise.
    pub nade_key: Option<Nade>,
}

/// Ce que le joueur a en main.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sel {
    Primary,
    Secondary,
    Knife,
    Nade(Nade),
}

/// Article du menu d'achat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuyItem {
    Weapon(usize),
    Nade(Nade),
    /// Gilet, gilet + casque, kit de désamorçage (`game::ITEM_…`).
    Gear(u16),
}

/// Équipement de la partie en ligne, tenu par l'hôte (repris de son HUD).
#[derive(Clone, Copy, Debug, Default)]
pub struct Online {
    pub team: u8,
    pub money: u32,
    pub armor: u8,
    pub helmet: bool,
    pub kit: bool,
    pub can_buy: bool,
    /// Échauffement : achats gratuits.
    pub warmup: bool,
}

const GEAR: [(u16, &str); 3] = [
    (minelib_net::game::ITEM_KEVLAR, "Gilet"),
    (minelib_net::game::ITEM_HELMET, "Gilet+casque"),
    (minelib_net::game::ITEM_KIT, "Kit (CT)"),
];

fn gear_price(item: u16, o: &Online) -> u32 {
    use minelib_net::game::{ITEM_HELMET, ITEM_KEVLAR};
    match item {
        ITEM_KEVLAR => crate::cs::KEVLAR_PRICE,
        ITEM_HELMET if o.armor >= 100 => crate::cs::HELMET_PRICE - crate::cs::KEVLAR_PRICE,
        ITEM_HELMET => crate::cs::HELMET_PRICE,
        _ => crate::cs::KIT_PRICE,
    }
}

/// Icône d'équipement du bas d'écran : ce qu'elle représente et son modèle.
type LoadoutIcon = (Sel, Vec<weapons::Tri>);

/// Temps d'armement du R8 avant que le coup parte (CS : environ 0,4 s).
const R8_PRIME: f32 = 0.4;
/// Gris des bouffées de fumée (couleur linéaire, multipliée par leur teinte).
/// Arme sortie : délai avant de pouvoir tirer, frapper ou lancer (s).
pub const DEPLOY_S: f32 = 0.75;

/// Rayon du cône de tir (degrés) : droit à l'arrêt (sauf plombs et armes à lunette sans
/// lunette), plus le mouvement (rien
/// jusqu'à 34 % de la vitesse de course, puis de plus en plus large avec la vitesse, sans
/// plafond), ×2,5 en l'air ; la lunette la divise par 4. `settled` : immobile au sol
/// depuis au moins [`SETTLE_S`].
/// Temps immobile après lequel le tir sans lunette se resserre.
pub const SETTLE_S: f32 = 0.2;

pub fn spread_deg(
    def: &WeaponDef,
    speed: f32,
    on_ground: bool,
    scoped: bool,
    settled: bool,
) -> f32 {
    let run = crate::player::FPS_SPRINT;
    let moving = ((speed - run * 0.34) / (run * 0.66)).max(0.0);
    // À l'arrêt, la balle part droit (seul le recul la dévie) ; exceptions : les plombs
    // des fusils à pompe, et les armes à lunette tirées sans lunette (cône dont le rayon
    // grandit avec la distance : 4° pour les fusils de précision, 1,5° pour AUG / SG 553).
    let base = if def.pellets > 1 {
        def.spread
    } else if !def.scope.is_empty() && !scoped {
        let cone = if def.scope.len() > 1 { 4.0 } else { 1.5 };
        // Immobile depuis 0,2 s : 60 % plus précis.
        if settled {
            cone * 0.4
        } else {
            cone
        }
    } else {
        0.0
    };
    let mut spread = base + def.category.move_inaccuracy() * moving;
    if !on_ground {
        spread = spread * 2.5 + def.category.move_inaccuracy() * 0.5;
    }
    if scoped {
        spread *= 0.25;
    }
    spread
}

pub struct Fps {
    /// Tirs de cette image pour le multijoueur : arme, origine, direction de chaque plomb.
    pub shots: Vec<(u8, Vec3, Vec<Vec3>)>,
    /// Marqueur de touche (s restantes) et tir à la tête.
    pub hit_marker: (f32, bool),
    pub hp: u32,
    /// Solde : 0 en solo (les achats ne le changent pas).
    pub money: u32,
    primary: Option<Gun>,
    secondary: Option<Gun>,
    use_primary: bool,
    cooldown: f32,
    /// Temps restant avant que l'arme sortie soit prête (voir `DEPLOY_S`).
    deploy: f32,
    reload: Option<f32>,
    /// Balles tirées dans la rafale en cours (motif de recul), et temps depuis le dernier tir.
    spray_n: u32,
    since_shot: f32,
    /// Recul de la vue (degrés) : vers le haut, vers la droite.
    pub punch: (f32, f32),
    /// Temps passé immobile au sol (s).
    still: f32,
    /// Le clic d'achat est encore tenu : pas de tir avant qu'il soit relâché.
    hold_fire: bool,
    /// Sons à jouer (délai en s, volume selon la distance), vidés par la session à chaque image.
    pub sounds: Vec<(crate::audio::Sfx, f32, f32)>,
    /// Imprécision actuelle (degrés), pour l'écart du viseur.
    cur_spread: f32,
    /// Lunette : 0 sans, sinon numéro du grossissement (1 = ×2, 2 = ×4).
    pub scoped: u8,
    /// Dernière arme achetée (racheter).
    last_bought: Option<usize>,
    tracers: Vec<Tracer>,
    holes: Vec<(Vec3, Vec3)>,
    sparks: Vec<(Vec3, Vec3, f32)>,
    flash: f32,
    kick: f32,
    pub buy_open: bool,
    rng: u32,
    /// En main : arme à feu, couteau ou grenade ; et ce qui était en main avant (touche Q).
    sel: Sel,
    prev: Sel,
    /// Grenades portées, dans l'ordre de `Nade::ALL`.
    pub nades: [u8; 5],
    thrown: Vec<Thrown>,
    effects: Vec<Effect>,
    /// Aveuglement (s restantes, 0..3) et sa force au départ.
    pub blind: f32,
    /// Couteau : temps avant le prochain coup, avancement du geste (1 → 0), coup lourd.
    knife_cd: f32,
    swing: f32,
    heavy: bool,
    /// Geste du lancer de grenade (1 → 0).
    toss: f32,
    /// R8 : armement du chien en cours (s).
    r8: f32,
    /// Dégâts en fraction de point (feu, explosion) pas encore retirés.
    damage_acc: f32,
    /// Partie en ligne : argent et équipement venus de l'hôte (les grenades ne blessent
    /// alors que par l'hôte).
    pub online: Option<Online>,
    /// Achats demandés à l'hôte (partie en ligne).
    pub requests: Vec<u16>,
    /// Grenades lancées à cette image : (type, position, vitesse), pour les autres joueurs.
    pub throws: Vec<(u8, Vec3, Vec3)>,
    /// Explosions de HE à cette image : (position, lanceur).
    pub blasts: Vec<(Vec3, u64)>,
    /// Armes lâchées à cette image (partie en ligne : elles vont au sol).
    pub dropped: Vec<u8>,
}

impl Default for Fps {
    fn default() -> Self {
        Self::new()
    }
}

impl Fps {
    pub fn new() -> Self {
        Self {
            hp: MAX_HP,
            money: 0,
            primary: None,
            secondary: Some(Gun::new(weapons::STARTER)),
            use_primary: false,
            cooldown: 0.0,
            deploy: 0.0,
            reload: None,
            spray_n: 0,
            since_shot: 10.0,
            punch: (0.0, 0.0),
            still: 0.0,
            hold_fire: false,
            sounds: Vec::new(),
            shots: Vec::new(),
            hit_marker: (0.0, false),
            cur_spread: 0.0,
            scoped: 0,
            last_bought: None,
            tracers: Vec::new(),
            holes: Vec::new(),
            sparks: Vec::new(),
            flash: 0.0,
            kick: 0.0,
            buy_open: false,
            rng: 0x9e37_79b9,
            sel: Sel::Secondary,
            prev: Sel::Knife,
            nades: [0; 5],
            thrown: Vec::new(),
            effects: Vec::new(),
            blind: 0.0,
            knife_cd: 0.0,
            swing: 0.0,
            heavy: false,
            toss: 0.0,
            r8: 0.0,
            damage_acc: 0.0,
            online: None,
            requests: Vec::new(),
            throws: Vec::new(),
            blasts: Vec::new(),
            dropped: Vec::new(),
        }
    }

    /// Armes portées (indices, 255 = aucune) : principale, secondaire.
    pub fn loadout_ids(&self) -> (u8, u8) {
        (
            self.primary.map_or(255, |g| g.def as u8),
            self.secondary.map_or(255, |g| g.def as u8),
        )
    }

    /// Partie en ligne : nouvelle manche. `reset` : mort ou changement de côté, on repart
    /// avec le pistolet de départ de son camp (Glock-18 pour les T, USP-S pour les CT).
    pub fn new_round(&mut self, reset: bool, team: u8) {
        if reset {
            self.respawn();
            let name = if team == minelib_net::game::T {
                "Glock-18"
            } else {
                "USP-S"
            };
            if let Some(i) = WEAPONS.iter().position(|w| w.name == name) {
                self.secondary = Some(Gun::new(i));
            }
            self.last_bought = None;
        } else {
            self.hp = MAX_HP;
            self.reload = None;
            self.scoped = 0;
            self.blind = 0.0;
            // Chargeurs pleins pour la nouvelle manche, comme dans CS.
            for g in [&mut self.primary, &mut self.secondary]
                .into_iter()
                .flatten()
            {
                g.mag = WEAPONS[g.def].magazine;
                g.reserve = WEAPONS[g.def].reserve;
            }
        }
        self.thrown.clear();
        self.effects.clear();
    }

    /// Objet acheté (accepté par l'hôte).
    pub fn give(&mut self, item: u16) {
        use minelib_net::game::ITEM_NADE;
        match item {
            n if (ITEM_NADE..ITEM_NADE + 5).contains(&n) => {
                self.buy_nade(Nade::ALL[(n - ITEM_NADE) as usize]);
            }
            w if (w as usize) < WEAPONS.len() => self.buy(w as usize),
            _ => {}
        }
    }

    /// Grenade lancée par un autre joueur (simulée ici aussi).
    pub fn add_thrown(&mut self, owner: u64, nade: u8, pos: Vec3, vel: Vec3) {
        if let Some(&kind) = Nade::ALL.get(nade as usize) {
            self.thrown.push(Thrown::new(kind, pos, vel, owner));
        }
    }

    /// Garant indépendant (sans joueur) : avance les grenades de la partie (dégâts).
    pub fn simulate_nades(&mut self, dt: f32, world: &World) {
        if self.online.is_none() {
            self.online = Some(Online::default());
        }
        self.tick_nades(dt, world, Vec3::new(0.0, -1000.0, 0.0), 0.0, 0.0);
    }

    /// Cases en feu : (position, lanceur).
    pub fn fire_cells(&self) -> Vec<(Vec3, u64)> {
        self.effects
            .iter()
            .filter_map(|e| match e {
                Effect::Fire(cells, _, owner) => Some(cells.iter().map(|c| (*c, *owner))),
                _ => None,
            })
            .flatten()
            .collect()
    }

    /// Arme en main (indice dans `WEAPONS`), 255 pour le couteau, les grenades ou rien.
    pub fn held_weapon(&self) -> u8 {
        self.active().map_or(255, |g| g.def as u8)
    }

    fn active(&self) -> Option<&Gun> {
        if !matches!(self.sel, Sel::Primary | Sel::Secondary) {
            return None;
        }
        if self.use_primary {
            self.primary.as_ref().or(self.secondary.as_ref())
        } else {
            self.secondary.as_ref().or(self.primary.as_ref())
        }
    }

    fn active_mut(&mut self) -> Option<&mut Gun> {
        if !matches!(self.sel, Sel::Primary | Sel::Secondary) {
            return None;
        }
        if self.use_primary && self.primary.is_some() {
            self.primary.as_mut()
        } else if self.secondary.is_some() {
            self.secondary.as_mut()
        } else {
            self.primary.as_mut()
        }
    }

    /// Souris libre (menu, pause) : le clic qui ramène en jeu ne doit pas tirer ; on
    /// attend qu'il soit relâché.
    pub fn block_fire(&mut self) {
        self.hold_fire = true;
    }

    /// Champ de vision imposé par la lunette, le cas échéant.
    pub fn scope_fov(&self) -> Option<f32> {
        let mag = self.scope_mag()?;
        let half = (crate::session::FOV.to_radians() / 2.0).tan() / mag;
        Some((2.0 * half.atan()).to_degrees())
    }

    /// Grossissement de la lunette en cours (×2, ×4), le cas échéant.
    pub fn scope_mag(&self) -> Option<f32> {
        let g = self.active()?;
        if self.reload.is_some() || self.scoped == 0 {
            return None;
        }
        WEAPONS[g.def].scope.get(self.scoped as usize - 1).copied()
    }

    /// Lunette de fusil de précision (cache noir) plutôt que viseur de fusil d'assaut.
    fn sniper_scope(&self) -> bool {
        self.active()
            .is_some_and(|g| WEAPONS[g.def].scope.len() > 1)
    }

    /// Prend l'arme principale (`true`) ou secondaire.
    fn switch(&mut self, primary: bool) {
        self.select(if primary {
            Sel::Primary
        } else {
            Sel::Secondary
        });
    }

    /// Met en main `sel` (s'il est porté) ; l'ancien choix devient la « dernière arme ».
    fn select(&mut self, sel: Sel) {
        let owned = match sel {
            Sel::Primary => self.primary.is_some(),
            Sel::Secondary => self.secondary.is_some(),
            Sel::Knife => true,
            Sel::Nade(n) => self.nades[n.index()] > 0,
        };
        if !owned || sel == self.sel {
            return;
        }
        self.prev = self.sel;
        self.sel = sel;
        // Arme sortie : son propre à chaque arme, et délai avant de pouvoir s'en servir.
        let sfx = match sel {
            Sel::Primary => self.primary.map(|g| Sfx::Draw(g.def)),
            Sel::Secondary => self.secondary.map(|g| Sfx::Draw(g.def)),
            Sel::Knife => Some(Sfx::DrawKnife),
            Sel::Nade(_) => Some(Sfx::DrawNade),
        };
        if let Some(sfx) = sfx {
            self.sounds.push((sfx, 0.0, 1.0));
        }
        self.deploy = DEPLOY_S;
        match sel {
            Sel::Primary => self.use_primary = true,
            Sel::Secondary => self.use_primary = false,
            _ => {}
        }
        self.reload = None;
        self.scoped = 0;
        self.r8 = 0.0;
        self.swing = 0.0;
        self.toss = 0.0;
    }

    /// Tout ce qu'on porte, dans l'ordre de la molette : principale, pistolet, couteau,
    /// grenades.
    fn loadout(&self) -> Vec<Sel> {
        let mut v = Vec::new();
        if self.primary.is_some() {
            v.push(Sel::Primary);
        }
        if self.secondary.is_some() {
            v.push(Sel::Secondary);
        }
        v.push(Sel::Knife);
        for n in Nade::ALL {
            if self.nades[n.index()] > 0 {
                v.push(Sel::Nade(n));
            }
        }
        v
    }

    /// Molette : arme suivante (`+1`) ou précédente (`-1`).
    fn cycle(&mut self, step: i32) {
        let l = self.loadout();
        let i = l.iter().position(|s| *s == self.sel).unwrap_or(0) as i32;
        let n = l.len() as i32;
        self.select(l[((i + step) % n + n) as usize % n as usize]);
    }

    /// Touche 4 : grenade suivante parmi celles portées.
    fn next_nade(&mut self) {
        let start = match self.sel {
            Sel::Nade(n) => n.index() + 1,
            _ => 0,
        };
        for k in 0..5 {
            let n = Nade::ALL[(start + k) % 5];
            if self.nades[n.index()] > 0 {
                self.select(Sel::Nade(n));
                return;
            }
        }
    }

    /// Nombre total de grenades portées.
    pub fn nade_count(&self) -> u8 {
        self.nades.iter().sum()
    }

    /// Peut-on acheter cette grenade (limites de CS) ?
    pub fn can_buy_nade(&self, n: Nade) -> bool {
        self.nades[n.index()] < n.max() && self.nade_count() < equip::MAX_NADES
    }

    /// Achète une grenade (gratuite en solo) si les limites le permettent.
    pub fn buy_nade(&mut self, n: Nade) -> bool {
        if !self.can_buy_nade(n) {
            return false;
        }
        self.nades[n.index()] += 1;
        true
    }

    /// Achète l'arme `i` (gratuit en solo : le solde n'est pas débité) et la prend en main.
    pub fn buy(&mut self, i: usize) {
        let gun = Some(Gun::new(i));
        let primary = WEAPONS[i].primary();
        if primary {
            self.primary = gun;
        } else {
            self.secondary = gun;
        }
        let target = if primary {
            Sel::Primary
        } else {
            Sel::Secondary
        };
        if self.sel == target {
            self.use_primary = primary;
        } else {
            self.select(target);
        }
        self.reload = None;
        self.scoped = 0;
        self.kick = 0.0;
        self.last_bought = Some(i);
    }

    /// Mort ou nouvelle partie : vie pleine, pistolet de départ.
    pub fn respawn(&mut self) {
        let (money, last) = (self.money, self.last_bought);
        *self = Self::new();
        self.money = money;
        self.last_bought = last;
    }

    /// Dégâts de chute façon FPS tactique : rien jusqu'à `safe` blocs, mortel vers 3 fois
    /// cette hauteur ; entre les deux, proportionnel à la vitesse d'impact.
    pub fn fall_damage(height: f32, safe: f32) -> u32 {
        if height <= safe {
            return 0;
        }
        let fatal = safe * 3.12;
        let k = (height.sqrt() - safe.sqrt()) / (fatal.sqrt() - safe.sqrt());
        // Dégâts divisés par 4 par rapport à la courbe de base.
        (k * MAX_HP as f32 * FALL_SCALE)
            .ceil()
            .clamp(0.0, MAX_HP as f32) as u32
    }

    fn random(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        (self.rng as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    /// Une image de jeu : tir, rechargement, recul, effets. `eye` / `yaw` / `pitch` : vue
    /// du joueur (sans le recul).
    pub fn update(
        &mut self,
        dt: f32,
        c: &Controls,
        world: &World,
        eye: Vec3,
        yaw: f32,
        pitch: f32,
    ) {
        // Peut devenir négatif : à faible nombre d'images par seconde, plusieurs balles
        // partent dans la même image et la cadence de l'arme est tenue.
        self.cooldown -= dt;
        self.deploy = (self.deploy - dt).max(0.0);
        self.since_shot += dt;
        if c.on_ground && c.speed < 0.05 {
            self.still += dt;
        } else {
            self.still = 0.0;
        }
        self.flash = (self.flash - dt).max(0.0);
        self.kick = (self.kick - dt * 6.0).max(0.0);
        match c.slot {
            Some(0) => self.select(Sel::Primary),
            Some(1) => self.select(Sel::Secondary),
            Some(2) => self.select(Sel::Knife),
            Some(3) => self.next_nade(),
            _ => {}
        }
        if let Some(n) = c.nade_key {
            self.select(Sel::Nade(n));
        }
        if c.last_weapon {
            let p = self.prev;
            self.select(p);
        }
        if c.next_weapon {
            self.cycle(1);
        }
        if c.wheel != 0 {
            self.cycle(c.wheel.signum());
        }
        // Grenades lancées, explosions, fumées, feu.
        self.tick_nades(dt, world, eye, yaw, pitch);
        // Clic qui a servi à acheter : rien ne part tant qu'il n'est pas relâché.
        if self.hold_fire && !c.trigger_down && !c.trigger_held {
            self.hold_fire = false;
        }
        // Arme en train de sortir : ni tir, ni coup, ni lancer, ni lunette, ni rechargement.
        let ready;
        let c = if self.deploy > 0.0 {
            ready = Controls {
                trigger_pressed: false,
                trigger_held: false,
                scope: false,
                reload: false,
                ..*c
            };
            &ready
        } else {
            c
        };
        if !matches!(self.sel, Sel::Primary | Sel::Secondary) {
            self.cur_spread = 0.0;
        }
        match self.sel {
            Sel::Knife => {
                self.knife(dt, c, world, eye, yaw, pitch);
                return;
            }
            Sel::Nade(n) => {
                self.nade(dt, c, n, eye, yaw, pitch);
                return;
            }
            _ => {}
        }
        if c.rebuy {
            if let Some(i) = self.last_bought {
                self.buy(i);
            }
        }
        // Lâcher : seulement l'arme principale (on garde toujours un pistolet).
        if c.drop && self.sel == Sel::Primary && self.primary.is_some() {
            if let Some(g) = self.primary {
                self.dropped.push(g.def as u8);
            }
            self.primary = None;
            self.switch(false);
        }
        // Rechargement.
        if let Some(t) = &mut self.reload {
            *t -= dt;
            if *t <= 0.0 {
                self.reload = None;
                self.sounds.push((Sfx::ReloadEnd, 0.0, 1.0));
                if let Some(g) = self.active_mut() {
                    let need = WEAPONS[g.def].magazine - g.mag;
                    let n = need.min(g.reserve);
                    g.mag += n;
                    g.reserve -= n;
                }
            }
        }
        let Some(gun) = self.active().copied() else {
            return;
        };
        let def = &WEAPONS[gun.def];
        self.cur_spread = spread_deg(
            def,
            c.speed,
            c.on_ground,
            self.scope_mag().is_some(),
            self.still >= SETTLE_S,
        );
        if c.reload && self.reload.is_none() && gun.mag < def.magazine && gun.reserve > 0 {
            self.reload = Some(def.reload_s);
            self.sounds.push((Sfx::Reload(gun.def), 0.0, 1.0));
            self.scoped = 0;
        }
        // Clic droit : ×2, (×4), puis retour sans lunette.
        if c.scope && !def.scope.is_empty() {
            self.scoped = (self.scoped + 1) % (def.scope.len() as u8 + 1);
            let s = if self.scoped == 0 {
                Sfx::ScopeOut
            } else {
                Sfx::ScopeIn
            };
            self.sounds.push((s, 0.0, 1.0));
        }
        // Rafale finie : le motif repart du début, la vue revient doucement.
        if self.since_shot > def.interval() * 1.8 + 0.12 {
            self.spray_n = 0;
        }
        // La vue revient vite après un tir au coup par coup (AWP : on ne patiente pas tout le
        // temps du réarmement), plus doucement entre les balles d'une rafale.
        let back = (dt * if def.auto { 9.0 } else { 14.0 }).min(1.0);
        if self.since_shot > (def.interval() * 1.2).min(0.12) {
            self.punch.0 -= self.punch.0 * back;
            self.punch.1 -= self.punch.1 * back;
        }

        // R8 : le coup ne part qu'après l'armement du chien (bouton tenu R8_PRIME), et
        // repart de zéro si on relâche ; clic droit : coup immédiat, très imprécis.
        if def.name == "R8 Revolver" && self.reload.is_none() && gun.mag > 0 {
            if c.scope && !self.hold_fire && self.cooldown <= 0.0 {
                self.fire_with(c, world, eye, yaw, pitch, 5.0);
                self.cooldown = def.interval() * 0.6;
                return;
            }
            if c.trigger_held && !self.hold_fire && self.cooldown <= 0.0 {
                if self.r8 == 0.0 {
                    // Le chien se lève : déclic qui accompagne l'animation.
                    self.sounds.push((Sfx::Hammer, 0.0, 1.0));
                }
                self.r8 += dt;
                if self.r8 >= R8_PRIME {
                    self.r8 = 0.0;
                    self.fire(c, world, eye, yaw, pitch);
                    self.cooldown = def.interval();
                }
            } else {
                self.r8 = (self.r8 - dt * 2.0).max(0.0);
            }
            self.cooldown = self.cooldown.max(0.0);
            return;
        }
        let wants = !self.hold_fire
            && if def.auto {
                c.trigger_held
            } else {
                c.trigger_pressed
            };
        if !wants || self.reload.is_some() {
            self.cooldown = self.cooldown.max(0.0);
            return;
        }
        // Au plus quelques balles par image (si l'image a été très longue).
        let mut shots = 0;
        while self.cooldown <= 0.0 && shots < 8 {
            let mag = self.active().map_or(0, |g| g.mag);
            if mag == 0 {
                if self.active().is_some_and(|g| g.reserve > 0) {
                    self.reload = Some(def.reload_s);
                    self.scoped = 0;
                    self.sounds.push((Sfx::Reload(gun.def), 0.0, 1.0));
                } else if c.trigger_pressed {
                    self.sounds.push((Sfx::Dry, 0.0, 1.0));
                }
                self.cooldown = self.cooldown.max(0.0);
                return;
            }
            self.fire(c, world, eye, yaw, pitch);
            self.cooldown += def.interval();
            shots += 1;
            if !def.auto {
                break;
            }
        }
        self.cooldown = self.cooldown.max(-def.interval());
    }

    /// Un tir : munition, recul (motif), imprécision, balles traçantes, impacts.
    fn fire(&mut self, c: &Controls, world: &World, eye: Vec3, yaw: f32, pitch: f32) {
        self.fire_with(c, world, eye, yaw, pitch, 0.0);
    }

    /// Un tir avec `extra` degrés d'imprécision en plus (R8 au clic droit).
    fn fire_with(
        &mut self,
        c: &Controls,
        world: &World,
        eye: Vec3,
        yaw: f32,
        pitch: f32,
        extra: f32,
    ) {
        let Some(gun) = self.active().copied() else {
            return;
        };
        let def = &WEAPONS[gun.def];
        self.sounds.push((Sfx::Shot(gun.def), 0.0, 1.0));
        if crate::audio::cycles_after_shot(gun.def) {
            self.sounds.push((Sfx::Cycle, 0.35, 1.0));
        }
        if let Some(g) = self.active_mut() {
            g.mag -= 1;
        }
        self.since_shot = 0.0;
        self.flash = 0.05;
        self.kick = 1.0;
        // Recul : la balle suit le motif ; la vue en montre environ la moitié.
        let (up, side) = def.spray(self.spray_n);
        self.spray_n += 1;
        let (nup, nside) = def.spray(self.spray_n);
        self.punch = (nup * 0.5, nside * 0.5);
        // Imprécision : comme dans CS, aucune pénalité jusqu'à 34 % de la vitesse de course
        // (marcher accroupi, s'arrêter avant de tirer) ; au-delà, le cône s'élargit avec la
        // vitesse, sans plafond (bunny hop : très imprécis). En l'air : ×2,5 en plus.
        let spread = spread_deg(
            def,
            c.speed,
            c.on_ground,
            self.scope_mag().is_some(),
            self.still >= SETTLE_S,
        ) + extra;
        let muzzle = self.muzzle_world(eye, yaw, pitch);
        let mut dirs = Vec::new();
        for _ in 0..def.pellets.max(1) {
            let r = spread.to_radians() * self.random().abs().sqrt();
            let a = self.random() * std::f32::consts::PI;
            let p = pitch + up.to_radians() + r * a.sin();
            let y = yaw + side.to_radians() + r * a.cos();
            let dir = Vec3::new(y.sin() * p.cos(), p.sin(), -y.cos() * p.cos());
            dirs.push(dir);
            let (dist, hit) = match raycast(world, eye.into(), dir.into(), RANGE) {
                Some(h) => (h.distance, Some(h)),
                None => (RANGE, None),
            };
            let target = eye + dir * dist;
            // La balle creuse un tunnel dans les fumigènes qu'elle traverse.
            for e in &mut self.effects {
                if let Effect::Smoke(cloud) = e {
                    cloud.carve(eye, dir, dist);
                }
            }
            let tdir = (target - muzzle).normalize_or_zero();
            self.tracers.push(Tracer {
                from: muzzle,
                dir: tdir,
                dist: (target - muzzle).length(),
                travelled: 0.0,
            });
            if let Some(h) = hit {
                let n = Vec3::new(h.normal[0] as f32, h.normal[1] as f32, h.normal[2] as f32);
                if self.holes.len() >= MAX_HOLES {
                    self.holes.remove(0);
                }
                self.holes.push((target + n * 0.004, n));
                for _ in 0..5 {
                    let v = n * 2.0 + Vec3::new(self.random(), self.random(), self.random()) * 2.5;
                    self.sparks.push((target + n * 0.02, v, 0.0));
                }
            }
        }
        self.shots.push((gun.def as u8, eye, dirs));
    }

    /// Son venant de `pos` : plus faible avec la distance.
    fn sound_at(&mut self, sfx: Sfx, delay: f32, pos: Vec3, eye: Vec3) {
        let d = (pos - eye).length();
        self.sounds.push((sfx, delay, 1.0 / (1.0 + d / 10.0)));
    }

    /// Grenades lancées et effets : vol, déclenchement, dégâts et aveuglement du joueur.
    fn tick_nades(&mut self, dt: f32, world: &World, eye: Vec3, yaw: f32, pitch: f32) {
        let look = look_dir(yaw, pitch);
        let mut sounds = Vec::new();
        let mut hits = equip::Hits::default();
        let mut thrown = std::mem::take(&mut self.thrown);
        let mut effects = std::mem::take(&mut self.effects);
        let mut seed = self.rng;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed as f32 / u32::MAX as f32) * 2.0 - 1.0
        };
        let mut blasts = Vec::new();
        thrown.retain_mut(|t| {
            equip::step_thrown(t, world, dt, &mut sounds);
            if equip::detonates(t, world) {
                // Hasard propre à la grenade : même fumée chez tous les joueurs.
                let mut tseed = t.seed;
                let mut trnd = || {
                    tseed ^= tseed << 13;
                    tseed ^= tseed >> 17;
                    tseed ^= tseed << 5;
                    (tseed as f32 / u32::MAX as f32) * 2.0 - 1.0
                };
                equip::detonate(
                    t,
                    world,
                    eye,
                    look,
                    &mut effects,
                    &mut sounds,
                    &mut hits,
                    &mut trnd,
                );
                if t.kind == Nade::He {
                    blasts.push((t.pos, t.owner));
                }
                false
            } else {
                true
            }
        });
        let feet = eye - Vec3::Y * crate::player::EYE;
        equip::tick_effects(&mut effects, dt, feet, &mut sounds, &mut hits, &mut rnd);
        self.rng = seed;
        self.thrown = thrown;
        self.effects = effects;
        for (sfx, delay, pos) in sounds {
            self.sound_at(sfx, delay, pos, eye);
        }
        self.blasts.extend(blasts);
        // En ligne, les dégâts des grenades viennent de l'hôte.
        if self.online.is_some() {
            hits.damage = 0.0;
        }
        if hits.damage > 0.0 {
            self.damage_acc += hits.damage;
            let whole = self.damage_acc.floor();
            self.damage_acc -= whole;
            self.hp = self.hp.saturating_sub(whole as u32);
        }
        if hits.flash > 0.0 {
            self.blind = self.blind.max(hits.flash * 3.5);
        }
        self.blind = (self.blind - dt).max(0.0);
    }

    /// Couteau : clic gauche coup rapide, clic droit coup lourd ; marque le bloc touché.
    fn knife(&mut self, dt: f32, c: &Controls, world: &World, eye: Vec3, yaw: f32, pitch: f32) {
        self.knife_cd -= dt;
        self.swing = (self.swing - dt / if self.heavy { 0.6 } else { 0.35 }).max(0.0);
        let light = !self.hold_fire && c.trigger_held && self.knife_cd <= 0.0;
        let heavy = !self.hold_fire && c.scope && self.knife_cd <= 0.0;
        if !(light || heavy) {
            return;
        }
        self.heavy = heavy;
        self.swing = 1.0;
        self.knife_cd = if heavy { 1.0 } else { 0.4 };
        let dir = look_dir(yaw, pitch);
        // En ligne : l'hôte juge si le coup touche quelqu'un.
        let code = if heavy {
            crate::netgame::KNIFE_HEAVY
        } else {
            crate::netgame::KNIFE_LIGHT
        };
        self.shots.push((code, eye, vec![dir]));
        match raycast(world, eye.into(), dir.into(), 2.0) {
            Some(h) => {
                let n = Vec3::new(h.normal[0] as f32, h.normal[1] as f32, h.normal[2] as f32);
                let p = eye + dir * h.distance;
                if self.holes.len() >= MAX_HOLES {
                    self.holes.remove(0);
                }
                self.holes.push((p + n * 0.004, n));
                for _ in 0..6 {
                    let v = n * 1.5 + Vec3::new(self.random(), self.random(), self.random()) * 2.0;
                    self.sparks.push((p + n * 0.02, v, 0.0));
                }
                self.sounds.push((Sfx::KnifeHit, 0.12, 1.0));
            }
            None => self.sounds.push((Sfx::KnifeSwing, 0.0, 1.0)),
        }
    }

    /// Grenade en main : clic gauche lancer fort, clic droit en cloche.
    fn nade(&mut self, dt: f32, c: &Controls, kind: Nade, eye: Vec3, yaw: f32, pitch: f32) {
        self.toss = (self.toss - dt / 0.45).max(0.0);
        if self.toss > 0.0 || self.hold_fire {
            return;
        }
        let lob = c.scope;
        if !(c.trigger_pressed || lob) {
            return;
        }
        let look = look_dir(yaw, pitch);
        let right = Vec3::new(yaw.cos(), 0.0, yaw.sin());
        let pos = eye + look * 0.3 + right * 0.15 - Vec3::Y * 0.1;
        let vel = equip::throw_velocity(look, lob, c.vel);
        self.thrown.push(Thrown::new(kind, pos, vel, 0));
        self.throws.push((kind.index() as u8, pos, vel));
        self.sounds.push((Sfx::Pin, 0.0, 1.0));
        self.sounds.push((Sfx::Throw, 0.05, 1.0));
        self.toss = 1.0;
        let i = kind.index();
        self.nades[i] -= 1;
        if self.nades[i] == 0 {
            // Plus de cette grenade : la suivante, sinon l'arme d'avant.
            self.sel = Sel::Knife;
            self.next_nade();
            if self.sel == Sel::Knife {
                let p = if self.primary.is_some() {
                    Sel::Primary
                } else {
                    Sel::Secondary
                };
                self.select(p);
            }
        }
    }

    /// Avance les balles et les étincelles (appelé à chaque image).
    pub fn tick_effects(&mut self, dt: f32) {
        for t in &mut self.tracers {
            t.travelled += BULLET_SPEED * dt;
        }
        self.tracers.retain(|t| t.travelled - TRACER_LEN < t.dist);
        for s in &mut self.sparks {
            s.2 += dt;
            s.1.y -= 12.0 * dt;
            s.0 += s.1 * dt;
        }
        self.sparks.retain(|s| s.2 < 0.35);
    }

    /// Position du bout du canon dans le monde (départ des balles traçantes).
    fn muzzle_world(&self, eye: Vec3, yaw: f32, pitch: f32) -> Vec3 {
        let Some(g) = self.active() else { return eye };
        let m = weapons::model(g.def);
        let cam = Mat4::from_rotation_y(-yaw) * Mat4::from_rotation_x(pitch);
        let local = self.gun_place(g.def) * m.muzzle.extend(1.0);
        eye + cam.transform_vector3(local.truncate())
    }

    /// Placement de l'arme dans le repère de la vue (droite, bas, devant).
    fn gun_place(&self, def: usize) -> Mat4 {
        let d = &WEAPONS[def];
        let reload = self
            .reload
            .map_or(0.0, |t| (t / d.reload_s).clamp(0.0, 1.0));
        let dip = (reload * std::f32::consts::PI).sin();
        let k = self.kick;
        let len = d.length;
        let scoped = self.scope_fov().is_some();
        // Arme en bas à droite, l'arrière à ~25 cm de l'œil, le canon vers le centre.
        let (x, y) = if scoped {
            (0.0, -0.08)
        } else {
            (0.2 + len * 0.04, -0.2 - len * 0.03)
        };
        // R8 : pendant l'armement, l'arme recule et pivote (le chien se lève), puis le coup
        // part d'un coup sec.
        let prime = (self.r8 / R8_PRIME).clamp(0.0, 1.0);
        Mat4::from_translation(Vec3::new(
            x,
            y - dip * 0.12,
            -(0.26 + len * 0.5) + k * 0.03 + prime * 0.04,
        )) * Mat4::from_rotation_x(k * 0.09 - dip * 0.6 + prime * 0.22)
            * Mat4::from_rotation_y(if scoped { 0.0 } else { 0.04 })
            * Mat4::from_rotation_z(-prime * 0.45)
    }

    /// Arme en main (triangles projetés), éclairée par `light` (couleur linéaire).
    pub fn viewmodel(&self, aspect: f32, light: [f32; 3]) -> Vec<VmVertex> {
        let mut out = Vec::new();
        // Arme qui sort : elle remonte du bas de l'écran en pivotant pendant `DEPLOY_S`.
        let k = (self.deploy / DEPLOY_S).clamp(0.0, 1.0);
        let k = k * k;
        let raise = Mat4::from_translation(Vec3::new(0.0, -0.22 * k, 0.04 * k))
            * Mat4::from_rotation_x(-0.9 * k);
        let proj =
            Mat4::perspective_infinite_reverse_rh(VIEW_FOV.to_radians(), aspect, 0.01) * raise;
        match self.sel {
            Sel::Knife => {
                // Couteau en bas à droite, lame vers l'avant ; geste : la lame balaie de
                // droite à gauche (coup rapide) ou part en avant (coup lourd).
                let t = 1.0 - self.swing;
                let arc = if self.swing > 0.0 {
                    (t * std::f32::consts::PI).sin()
                } else {
                    0.0
                };
                let m = if self.heavy {
                    Mat4::from_translation(Vec3::new(
                        0.16 - arc * 0.08,
                        -0.16 + arc * 0.04,
                        -0.3 - arc * 0.18,
                    )) * Mat4::from_rotation_x(-0.25 + arc * 0.3)
                } else {
                    Mat4::from_translation(Vec3::new(0.18 - arc * 0.2, -0.17 + arc * 0.05, -0.3))
                        * Mat4::from_rotation_y(0.35 + arc * 0.9)
                        * Mat4::from_rotation_z(-arc * 0.6)
                        * Mat4::from_rotation_x(-0.2)
                };
                draw_tris(&mut out, proj, m, &weapons::knife_model().tris, light);
                return out;
            }
            Sel::Nade(n) => {
                // Grenade dans la main ; au lancer, elle part vers l'avant et disparaît.
                if self.toss > 0.5 {
                    return out;
                }
                let up = self.toss * 2.0;
                let m = Mat4::from_translation(Vec3::new(0.17, -0.16 - up * 0.1, -0.28))
                    * Mat4::from_rotation_z(0.25);
                draw_tris(&mut out, proj, m, &equip::nade_model(n), light);
                return out;
            }
            _ => {}
        }
        let Some(g) = self.active() else { return out };
        // Lunette : l'arme disparaît (on regarde dans la lunette).
        if self.scope_fov().is_some() {
            return out;
        }
        let m = self.gun_place(g.def);
        let mvp = proj * m;
        let sun = Vec3::new(0.3, 0.9, 0.4).normalize();
        for t in &weapons::model(g.def).tris {
            let n = (t.p[1] - t.p[0]).cross(t.p[2] - t.p[0]).normalize_or_zero();
            let n = m.transform_vector3(n);
            let shade = 0.45 + 0.55 * n.dot(sun).abs();
            // Couleurs très sombres du pack (métal noir) : un peu relevées pour rester lisibles.
            let c = t.color.map(|v| (v * 2.2 + 0.015) * shade);
            for p in t.p {
                out.push(VmVertex {
                    clip: (mvp * p.extend(1.0)).to_array(),
                    uv: [0.0, 0.0],
                    layer: NO_TEXTURE,
                    _pad: 0,
                    color: [c[0] * light[0], c[1] * light[1], c[2] * light[2], 1.0],
                });
            }
        }
        // Flamme du canon, juste après un tir.
        if self.flash > 0.0 {
            let tip = m.transform_point3(weapons::model(g.def).muzzle);
            // Étoile de deux carrés croisés, jaune orangé.
            let s = 0.018 + 0.012 * (self.flash / 0.05);
            let c = [2.4, 1.5, 0.4, 1.0];
            let d = std::f32::consts::FRAC_1_SQRT_2;
            quad(&mut out, proj, tip, Vec3::X * s, Vec3::Y * s, c);
            quad(
                &mut out,
                proj,
                tip,
                Vec3::new(d, d, 0.0) * s * 1.4,
                Vec3::new(-d, d, 0.0) * s * 0.5,
                c,
            );
        }
        out
    }

    /// Balles traçantes, étincelles et impacts (dans le monde) : triangles projetés avec la
    /// caméra du jeu. Les impacts cachés par un bloc (vérifié par un rayon) ne sont pas dessinés.
    /// `daylight` : 0 la nuit, 1 le jour (la fumée est éclairée comme la scène).
    pub fn world_fx(
        &self,
        view_proj: Mat4,
        eye: Vec3,
        world: &World,
        _daylight: f32,
    ) -> Vec<VmVertex> {
        let mut out = Vec::new();
        let cam_right = |p: Vec3, d: Vec3| d.cross(p - eye).normalize_or_zero();
        for t in &self.tracers {
            let head = t.travelled.min(t.dist);
            let tail = (t.travelled - TRACER_LEN).max(0.0);
            if head <= tail {
                continue;
            }
            let a = t.from + t.dir * tail;
            let b = t.from + t.dir * head;
            let w = cam_right((a + b) * 0.5, t.dir) * 0.012;
            push_tri(
                &mut out,
                view_proj,
                [a - w, b - w, b + w],
                [3.0, 2.4, 1.2, 1.0],
            );
            push_tri(
                &mut out,
                view_proj,
                [b + w, a + w, a - w],
                [3.0, 2.4, 1.2, 1.0],
            );
        }
        for (p, v, age) in &self.sparks {
            let s = 0.025 * (1.0 - age / 0.35);
            let d = v.normalize_or_zero();
            let w = cam_right(*p, d) * s;
            let tail = *p - d * s * 3.0;
            push_tri(
                &mut out,
                view_proj,
                [tail, *p - w, *p + w],
                [4.0, 2.6, 0.8, 1.0],
            );
        }
        // Grenades en vol.
        for t in &self.thrown {
            let spin = Mat4::from_rotation_x(t.age * 9.0) * Mat4::from_rotation_y(t.age * 5.0);
            let m = Mat4::from_translation(t.pos) * spin;
            for tri in equip::nade_model(t.kind) {
                let c = tri.color.map(|v| v * 2.0 + 0.02);
                push_tri(
                    &mut out,
                    view_proj,
                    tri.p.map(|p| m.transform_point3(p)),
                    [c[0], c[1], c[2], 1.0],
                );
            }
        }
        let seen = |p: Vec3| {
            let to = p - eye;
            let d = to.length();
            d < 0.5 || raycast(world, eye.into(), (to / d).into(), d - 0.3).is_none()
        };
        let cam = |p: Vec3| {
            let f = (p - eye).normalize_or_zero();
            let r = f.cross(Vec3::Y).normalize_or_zero();
            (r, r.cross(f))
        };
        for e in &self.effects {
            match e {
                Effect::Blast(p, a) => {
                    // Boule de feu ronde qui grossit puis s'éteint (éventail de triangles,
                    // cœur blanc-jaune, bord orange).
                    let c0 = *p + Vec3::Y * 0.4;
                    if !seen(c0) {
                        continue;
                    }
                    let k = (a / 0.6).clamp(0.0, 1.0);
                    let r = 0.5 + k * 2.2;
                    let (u, v) = cam(c0);
                    let fade = 1.0 - k;
                    let core = [3.0 * fade, 2.4 * fade, 1.2 * fade, 1.0];
                    let rim = [2.2 * fade, 0.7 * fade, 0.1 * fade, 1.0];
                    let n = 14;
                    for i in 0..n {
                        let a0 = i as f32 / n as f32 * std::f32::consts::TAU;
                        let a1 = (i + 1) as f32 / n as f32 * std::f32::consts::TAU;
                        let wob = |t: f32| 1.0 + 0.18 * (t * 3.0 + a * 20.0).sin();
                        let e0 = c0 + (u * a0.cos() + v * a0.sin()) * r * wob(a0);
                        let e1 = c0 + (u * a1.cos() + v * a1.sin()) * r * wob(a1);
                        let m0 = c0 + (u * a0.cos() + v * a0.sin()) * r * 0.45;
                        let m1 = c0 + (u * a1.cos() + v * a1.sin()) * r * 0.45;
                        push_tri(&mut out, view_proj, [c0, m0, m1], core);
                        push_tri(&mut out, view_proj, [m0, e0, e1], rim);
                        push_tri(&mut out, view_proj, [e1, m1, m0], rim);
                    }
                }
                // Fumigène : dessinée en volume par la carte graphique (voir `smokes`).
                Effect::Smoke(_) => {}
                Effect::Fire(cells, a, _) => {
                    let fade = ((7.0 - a) / 1.0).clamp(0.0, 1.0);
                    for (i, c) in cells.iter().enumerate() {
                        if !seen(*c + Vec3::Y * 0.3) {
                            continue;
                        }
                        // Deux flammes croisées par case, qui ondulent.
                        let h = (0.5 + 0.25 * ((a * 9.0 + i as f32 * 1.7).sin())) * fade;
                        let col = [4.0, 1.6 + 0.4 * (a * 13.0 + i as f32).sin(), 0.2, 1.0];
                        for (dx, dz) in [(0.45, 0.0), (0.0, 0.45)] {
                            let w = Vec3::new(dx, 0.0, dz);
                            let top = *c + Vec3::Y * h;
                            push_tri(&mut out, view_proj, [*c - w, *c + w, top], col);
                        }
                    }
                }
                Effect::Decoy(..) => {}
            }
        }
        for (p, n) in &self.holes {
            // Caché derrière un bloc : on ne le dessine pas (il serait vu à travers).
            let to = *p - eye;
            let dist = to.length();
            if dist > 64.0 {
                continue;
            }
            if let Some(h) = raycast(world, eye.into(), (to / dist).into(), dist - 0.05) {
                if h.distance < dist - 0.06 {
                    continue;
                }
            }
            // Caché par une fumigène (derrière ou dedans).
            if self
                .effects
                .iter()
                .any(|e| matches!(e, Effect::Smoke(c) if c.hides(eye, *p)))
            {
                continue;
            }
            let u = if n.y.abs() > 0.5 { Vec3::X } else { Vec3::Y };
            let v = n.cross(u);
            let s = 0.045;
            let c = [0.01, 0.01, 0.01, 1.0];
            let (a, b, cc, d) = (
                *p - u * s - v * s,
                *p + u * s - v * s,
                *p + u * s + v * s,
                *p - u * s + v * s,
            );
            push_tri(&mut out, view_proj, [a, b, cc], c);
            push_tri(&mut out, view_proj, [cc, d, a], c);
        }
        out
    }

    // ------------------------------------------------------------ interface

    /// Vie, munitions, solde ; menu d'achat s'il est ouvert.
    /// `debug` : écran F3 affiché (il passe par-dessus le solde, qui n'est pas dessiné).
    pub fn hud(&self, ui: &mut Ui, size: (f32, f32), cursor: Option<(f32, f32)>, debug: bool) {
        let s = (size.1 / 360.0).floor().max(1.0);
        let col = |c: [u8; 3], a: f32| {
            let l = |v: u8| (v as f32 / 255.0).powf(2.2);
            [l(c[0]), l(c[1]), l(c[2]), a]
        };
        // Vie : en bas à gauche, « 100 » et une barre.
        let (x, y) = (8.0 * s, size.1 - 22.0 * s);
        ui.rect(
            x - 2.0 * s,
            y - 2.0 * s,
            88.0 * s,
            18.0 * s,
            [0.0, 0.0, 0.0, 0.45],
        );
        let hpc = if self.hp > 50 {
            [235, 235, 225]
        } else if self.hp > 20 {
            [240, 200, 80]
        } else {
            [240, 70, 60]
        };
        ui.text(x, y, s * 1.5, col(hpc, 1.0), format!("{}", self.hp));
        let bw = 40.0 * s;
        ui.rect(
            x + 42.0 * s,
            y + 4.0 * s,
            bw,
            5.0 * s,
            [0.05, 0.05, 0.05, 0.8],
        );
        ui.rect(
            x + 42.0 * s,
            y + 4.0 * s,
            bw * self.hp as f32 / MAX_HP as f32,
            5.0 * s,
            col(hpc, 1.0),
        );
        // Couteau, grenade : nom en bas à droite ; grenades portées au-dessus.
        let held = match self.sel {
            Sel::Knife => Some("Couteau".to_string()),
            Sel::Nade(n) => Some(format!("{}   x{}", n.name(), self.nades[n.index()])),
            _ => None,
        };
        if let Some(t) = held {
            let w = Ui::text_width(&t, s * 1.5);
            ui.rect(
                size.0 - w - 12.0 * s,
                y - 2.0 * s,
                w + 8.0 * s,
                18.0 * s,
                [0.0, 0.0, 0.0, 0.45],
            );
            ui.text(
                size.0 - w - 8.0 * s,
                y,
                s * 1.5,
                col([235, 235, 225], 1.0),
                t,
            );
        }
        // Équipement porté, en petit en bas à droite (comme CS) : modèles 3D, celui en main
        // en surbrillance, numéro de l'emplacement ; grenades sur une même ligne.
        self.draw_loadout(ui, size, s, y - 6.0 * s, &col);
        // Munitions : en bas à droite.
        if let Some(g) = self.active() {
            let d = &WEAPONS[g.def];
            let t = if self.reload.is_some() {
                format!("{}   recharge...", d.name)
            } else {
                format!("{}   {} / {}", d.name, g.mag, g.reserve)
            };
            let w = Ui::text_width(&t, s * 1.5);
            ui.rect(
                size.0 - w - 12.0 * s,
                y - 2.0 * s,
                w + 8.0 * s,
                18.0 * s,
                [0.0, 0.0, 0.0, 0.45],
            );
            let c = if g.mag == 0 {
                [240, 70, 60]
            } else {
                [235, 235, 225]
            };
            ui.text(size.0 - w - 8.0 * s, y, s * 1.5, col(c, 1.0), t);
        }
        self.draw_crosshair(ui, size, s);
        // R8 : armement du chien, petits points qui s'allument autour du viseur.
        if self.r8 > 0.0 {
            let k = (self.r8 / R8_PRIME).clamp(0.0, 1.0);
            let (cx, cy) = (size.0 / 2.0, size.1 / 2.0);
            let n = 12;
            for i in 0..n {
                let a = i as f32 / n as f32 * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
                let on = (i as f32) < k * n as f32;
                let r = 12.0 * s;
                let c = if on {
                    [1.0, 0.75, 0.2, 0.95]
                } else {
                    [0.0, 0.0, 0.0, 0.35]
                };
                ui.rect(
                    cx + a.cos() * r - s,
                    cy + a.sin() * r - s,
                    2.0 * s,
                    2.0 * s,
                    c,
                );
            }
        }
        // Solde : en haut à gauche (sous l'écran F3 quand il est ouvert).
        if !debug {
            ui.text(
                8.0 * s,
                8.0 * s,
                s * 1.5,
                col([120, 220, 120], 1.0),
                format!("{} $", self.online.map_or(self.money, |o| o.money)),
            );
        }
        // Gilet (et casque), kit : à côté de la vie.
        if let Some(o) = self.online {
            let mut t = String::new();
            if o.armor > 0 {
                t += &format!(
                    "Gilet {}{}",
                    o.armor,
                    if o.helmet { " + casque" } else { "" }
                );
            }
            if o.kit {
                if !t.is_empty() {
                    t += "   ";
                }
                t += "Kit";
            }
            if !t.is_empty() {
                ui.text(8.0 * s, size.1 - 32.0 * s, s, col([150, 190, 235], 1.0), t);
            }
        }
        // Viseur de fusil d'assaut (AUG, SG 553) : cercle assombri autour, point rouge.
        if self.scope_mag().is_some() && !self.sniper_scope() {
            let r = size.1 * 0.36;
            let (cx, cy) = (size.0 / 2.0, size.1 / 2.0);
            let shade = [0.0, 0.0, 0.0, 0.55];
            ui.rect(0.0, 0.0, cx - r, size.1, shade);
            ui.rect(cx + r, 0.0, size.0 - cx - r, size.1, shade);
            let steps = 40;
            for i in 0..steps {
                let y0 = cy - r + 2.0 * r * i as f32 / steps as f32;
                let y1 = y0 + 2.0 * r / steps as f32;
                let ym = ((y0 + y1) / 2.0 - cy) / r;
                let half = r * (1.0 - ym * ym).max(0.0).sqrt();
                ui.rect(cx - r, y0, r - half, y1 - y0, shade);
                ui.rect(cx + half, y0, r - half, y1 - y0, shade);
            }
            ui.rect(0.0, 0.0, size.0, cy - r, shade);
            ui.rect(0.0, cy + r, size.0, size.1 - cy - r, shade);
            ui.rect(cx - s, cy - s, 2.0 * s, 2.0 * s, [0.9, 0.05, 0.05, 1.0]);
        }
        // Lunette de précision : cache noir autour d'un cercle (approché par des bandes).
        if self.scope_mag().is_some() && self.sniper_scope() {
            let r = size.1 * 0.46;
            let (cx, cy) = (size.0 / 2.0, size.1 / 2.0);
            ui.rect(0.0, 0.0, cx - r, size.1, [0.0, 0.0, 0.0, 1.0]);
            ui.rect(cx + r, 0.0, size.0 - cx - r, size.1, [0.0, 0.0, 0.0, 1.0]);
            // Bandes noires au-dessus et en dessous du cercle.
            ui.rect(cx - r, 0.0, 2.0 * r, cy - r + 1.0, [0.0, 0.0, 0.0, 1.0]);
            ui.rect(
                cx - r,
                cy + r - 1.0,
                2.0 * r,
                size.1 - cy - r + 1.0,
                [0.0, 0.0, 0.0, 1.0],
            );
            // Bandes de 2 pixels ; chacune s'arrête au bord du cercle le plus proche du
            // centre de la bande vers l'extérieur (aucun trou dans les coins).
            let steps = (r.ceil() as usize).max(48);
            for i in 0..steps {
                let y0 = cy - r + 2.0 * r * i as f32 / steps as f32;
                let y1 = y0 + 2.0 * r / steps as f32;
                let ym = ((y0 - cy).abs().max((y1 - cy).abs()) / r).min(1.0);
                let half = r * (1.0 - ym * ym).max(0.0).sqrt();
                ui.rect(cx - r, y0, r - half, y1 - y0, [0.0, 0.0, 0.0, 1.0]);
                ui.rect(cx + half, y0, r - half, y1 - y0, [0.0, 0.0, 0.0, 1.0]);
            }
            ui.rect(0.0, cy - 0.5 * s, size.0, s, [0.0, 0.0, 0.0, 1.0]);
            ui.rect(cx - 0.5 * s, 0.0, s, size.1, [0.0, 0.0, 0.0, 1.0]);
        }
        if self.buy_open {
            self.draw_buy(ui, size, cursor, s, &col);
        }
    }

    fn draw_loadout(
        &self,
        ui: &mut Ui,
        size: (f32, f32),
        s: f32,
        bottom: f32,
        col: &dyn Fn([u8; 3], f32) -> [f32; 4],
    ) {
        let (w, h) = (104.0 * s, 25.0 * s);
        let gap = 2.0 * s;
        let x = size.0 - w - 8.0 * s;
        let mut rows: Vec<(String, Vec<LoadoutIcon>)> = Vec::new();
        if let Some(g) = self.primary {
            rows.push((
                "1".into(),
                vec![(Sel::Primary, weapons::model(g.def).tris.clone())],
            ));
        }
        if let Some(g) = self.secondary {
            rows.push((
                "2".into(),
                vec![(Sel::Secondary, weapons::model(g.def).tris.clone())],
            ));
        }
        rows.push((
            "3".into(),
            vec![(Sel::Knife, weapons::knife_model().tris.clone())],
        ));
        // Une icône par type de grenade (les 2 flashs sont empilées : « x2 »).
        let nades: Vec<LoadoutIcon> = Nade::ALL
            .into_iter()
            .filter(|n| self.nades[n.index()] > 0)
            .map(|n| (Sel::Nade(n), equip::nade_model(n)))
            .collect();
        if !nades.is_empty() {
            rows.push(("4".into(), nades));
        }
        let mut y = bottom - rows.len() as f32 * (h + gap);
        for (num, items) in rows {
            let active = items.iter().any(|(sel, _)| *sel == self.sel);
            let a = if active { 0.55 } else { 0.3 };
            ui.rect(x, y, w, h, [0.0, 0.0, 0.0, a]);
            if active {
                ui.rect(x, y, 2.0 * s, h, col([235, 200, 90], 1.0));
            }
            let tc = if active {
                [240, 240, 230]
            } else {
                [150, 150, 145]
            };
            ui.text(x + 5.0 * s, y + h * 0.5 - 4.0 * s, s, col(tc, 1.0), num);
            let left = x + 16.0 * s;
            let avail = w - 18.0 * s;
            if let [(_, tris)] = items.as_slice() {
                // Arme : une grande icône.
                let mut v = icon_3d(tris, left + avail * 0.5, y + h * 0.5, avail * 0.9, h * 0.8);
                if !active {
                    dim(&mut v, 0.45);
                }
                ui.model(v);
            } else {
                // Grenades : 4 cases fixes (4 types au plus), une icône par type avec sa
                // couleur repère dessous, ou « x2 » quand on en porte deux.
                let slot = avail / equip::MAX_NADES as f32;
                for (k, (sel, tris)) in items.iter().enumerate() {
                    let Sel::Nade(n) = *sel else { continue };
                    let cx = left + slot * (k as f32 + 0.5);
                    let held = *sel == self.sel;
                    let mut v = icon_3d(tris, cx, y + h * 0.38, slot * 0.8, h * 0.58);
                    if !held {
                        dim(&mut v, if active { 0.7 } else { 0.45 });
                    }
                    ui.model(v);
                    let alpha = if held { 1.0 } else { 0.55 };
                    let count = self.nades[n.index()];
                    if count > 1 {
                        let t = format!("x{count}");
                        let tw = Ui::text_width(&t, s);
                        ui.text(
                            cx - tw * 0.5,
                            y + h - 8.5 * s,
                            s,
                            col(n.hud_color(), alpha),
                            t,
                        );
                    } else {
                        let bw = slot * 0.55;
                        ui.rect(
                            cx - bw * 0.5,
                            y + h - 4.5 * s,
                            bw,
                            2.0 * s,
                            col(n.hud_color(), alpha),
                        );
                    }
                }
            }
            y += h + gap;
        }
    }

    /// Essais : fumigène posée directement en `p` (variable MINELIB_SMOKE).
    pub fn spawn_test_smoke(&mut self, p: Vec3, world: &World) {
        let mut rnd = || 0.5;
        // Posée au sol, comme une grenade qui a fini de rouler.
        let mut p = p;
        let solid = |p: Vec3| {
            world
                .get(
                    p.x.floor() as i32,
                    p.y.floor() as i32 - 1,
                    p.z.floor() as i32,
                )
                .is_solid()
        };
        for _ in 0..64 {
            if solid(p) {
                break;
            }
            p.y -= 1.0;
        }
        p.y = p.y.floor() + 0.05;
        let c = equip::SmokeCloud::new(p, world, &mut rnd);
        self.effects.push(Effect::Smoke(c));
    }

    /// Fumigènes en cours, pour le rendu en volume.
    pub fn smoke_volumes(&self) -> Vec<minelib_render::SmokeVolume<'_>> {
        self.effects
            .iter()
            .filter_map(|e| match e {
                Effect::Smoke(c) => Some(minelib_render::SmokeVolume {
                    id: c.id,
                    origin: c.origin,
                    cell: equip::SMOKE_CELL,
                    dims: c.dims,
                    cells: &c.cells,
                    grow: equip::smoke_grow(c.age),
                    age: c.age,
                    tunnels: c
                        .tunnels
                        .iter()
                        .map(|t| (t.a, t.b, equip::tunnel_radius(t.age)))
                        .collect(),
                    bounds: c.bounds,
                }),
                _ => None,
            })
            .collect()
    }

    /// Effets plein écran : flash (blanc qui s'estompe) et fumée quand on est dedans.
    /// `eye` : position de l'œil.
    pub fn screen_fx(&self, ui: &mut Ui, size: (f32, f32), _eye: Vec3) {
        if self.blind > 0.0 {
            // Totalement blanc tant qu'il reste plus d'une seconde, puis s'éclaircit.
            let a = self.blind.min(1.0);
            ui.rect(0.0, 0.0, size.0, size.1, [1.0, 1.0, 1.0, a]);
        }
    }

    /// Viseur façon CS:GO (classique dynamique, vert) : quatre traits autour du centre,
    /// écartés selon l'imprécision du moment (course, saut). Aucun viseur avec les fusils
    /// de précision (AWP, SSG 08, G3SG1, SCAR-20) : on vise à la lunette ou au jugé.
    fn draw_crosshair(&self, ui: &mut Ui, size: (f32, f32), s: f32) {
        if self.sniper_scope() || self.scope_mag().is_some() || self.buy_open {
            return;
        }
        let (cx, cy) = ((size.0 / 2.0).round(), (size.1 / 2.0).round());
        // Écart en pixels : le cône d'imprécision projeté à l'écran.
        let focal = (size.1 / 2.0) / (crate::session::FOV.to_radians() / 2.0).tan();
        let spread_px = (self.cur_spread.to_radians().tan() * focal).min(size.1 * 0.2);
        let gap = (1.5 * s + spread_px).round();
        let len = (3.5 * s).round();
        let t = (s * 0.67).round().max(1.0);
        let green = [0.0, 0.85, 0.05, 1.0];
        let outline = [0.0, 0.0, 0.0, 0.6];
        // Marqueur de touche : petite croix en diagonale (rouge à la tête).
        if self.hit_marker.0 > 0.0 {
            let c = if self.hit_marker.1 {
                [1.0, 0.25, 0.2, 1.0]
            } else {
                [1.0, 1.0, 1.0, 1.0]
            };
            let a = (self.hit_marker.0 / 0.25).min(1.0);
            let c = [c[0], c[1], c[2], a];
            for k in 0..4 {
                let (dx, dy) = [(1.0, 1.0), (-1.0, 1.0), (1.0, -1.0), (-1.0, -1.0)][k];
                for i in 0..4 {
                    let d = (4.0 + i as f32) * s;
                    ui.rect(cx + dx * d - s / 2.0, cy + dy * d - s / 2.0, s, s, c);
                }
            }
        }
        let half = (t / 2.0).floor();
        let bars = [
            (cx - gap - len, cy - half, len, t),
            (cx + gap, cy - half, len, t),
            (cx - half, cy - gap - len, t, len),
            (cx - half, cy + gap, t, len),
        ];
        for (x, y, w, h) in bars {
            ui.rect(x - 1.0, y - 1.0, w + 2.0, h + 2.0, outline);
        }
        for (x, y, w, h) in bars {
            ui.rect(x, y, w, h, green);
        }
    }

    /// Dimensions du menu d'achat : coin, largeur, hauteur, échelle, largeur et hauteur
    /// d'une case.
    fn buy_layout(size: (f32, f32)) -> (f32, f32, f32, f32, f32, f32, f32) {
        let s = (size.1 / 360.0).floor().max(1.0);
        let cw = 150.0 * s;
        let rh = 20.0 * s;
        let w = cw * 4.0 + 16.0 * s;
        let rows = Self::buy_cells_rows();
        let h = 30.0 * s + rh * rows as f32 + 12.0 * s;
        ((size.0 - w) / 2.0, (size.1 - h) / 2.0, w, h, s, cw, rh)
    }

    /// Colonne et ligne de chaque article : armes par catégorie, et l'équipement (grenades)
    /// sous les armes lourdes, après une ligne de titre.
    fn buy_grid() -> Vec<(BuyItem, usize, usize)> {
        let mut v = Vec::new();
        for (ci, cat) in Category::ALL.into_iter().enumerate() {
            for (k, (i, _)) in WEAPONS
                .iter()
                .enumerate()
                .filter(|(_, d)| d.category == cat)
                .enumerate()
            {
                v.push((BuyItem::Weapon(i), ci, k));
            }
        }
        let heavy = Category::ALL
            .iter()
            .position(|c| *c == Category::Heavy)
            .unwrap_or(2);
        let below = WEAPONS
            .iter()
            .filter(|d| d.category == Category::Heavy)
            .count()
            + 1;
        for (k, n) in Nade::ALL.into_iter().enumerate() {
            v.push((BuyItem::Nade(n), heavy, below + k));
        }
        for (k, (g, _)) in GEAR.into_iter().enumerate() {
            v.push((BuyItem::Gear(g), heavy, below + Nade::ALL.len() + k));
        }
        v
    }

    /// Prix, possédé, achetable (camp, argent, temps et zone d'achat en ligne).
    fn buy_state(&self, item: BuyItem) -> (u32, bool, bool) {
        let (price, owned, mut ok) = match item {
            BuyItem::Weapon(i) => (
                WEAPONS[i].price,
                self.primary.is_some_and(|g| g.def == i)
                    || self.secondary.is_some_and(|g| g.def == i),
                true,
            ),
            BuyItem::Nade(n) => (n.price(), self.nades[n.index()] > 0, self.can_buy_nade(n)),
            BuyItem::Gear(g) => {
                let o = self.online.unwrap_or_default();
                let owned = match g {
                    minelib_net::game::ITEM_KEVLAR => o.armor >= 100,
                    minelib_net::game::ITEM_HELMET => o.armor >= 100 && o.helmet,
                    _ => o.kit,
                };
                let side_ok = g != minelib_net::game::ITEM_KIT || o.team == minelib_net::game::CT;
                (
                    gear_price(g, &o),
                    owned,
                    side_ok && !owned && self.online.is_some(),
                )
            }
        };
        if let Some(o) = self.online {
            if let BuyItem::Weapon(i) = item {
                ok &= WEAPONS[i].side().is_none_or(|s| s == o.team);
            }
            ok &= o.can_buy && (o.warmup || o.money >= price);
        }
        (price, owned, ok)
    }

    fn item_code(item: BuyItem) -> u16 {
        match item {
            BuyItem::Weapon(i) => i as u16,
            BuyItem::Nade(n) => minelib_net::game::ITEM_NADE + n.index() as u16,
            BuyItem::Gear(g) => g,
        }
    }

    fn buy_cells_rows() -> usize {
        Self::buy_grid().iter().map(|c| c.2 + 1).max().unwrap_or(10)
    }

    /// Article du menu d'achat sous le curseur.
    fn buy_at(size: (f32, f32), x: f32, y: f32) -> Option<BuyItem> {
        let (px, py, _, _, s, cw, rh) = Self::buy_layout(size);
        Self::buy_grid().into_iter().find_map(|(it, ci, k)| {
            let (bx, by) = (px + 8.0 * s + ci as f32 * cw, py + 28.0 * s + k as f32 * rh);
            (x >= bx && x < bx + cw - 4.0 * s && y >= by && y < by + rh - 2.0 * s).then_some(it)
        })
    }

    /// Clic dans le menu d'achat.
    pub fn buy_click(&mut self, size: (f32, f32), cursor: (f32, f32)) {
        // En ligne : la demande part à l'hôte, qui vérifie l'argent et donne l'objet.
        if self.online.is_some() {
            if let Some(item) = Self::buy_at(size, cursor.0, cursor.1) {
                if self.buy_state(item).2 {
                    self.requests.push(Self::item_code(item));
                    if matches!(item, BuyItem::Weapon(_)) {
                        self.buy_open = false;
                    }
                }
                self.hold_fire = true;
            }
            return;
        }
        match Self::buy_at(size, cursor.0, cursor.1) {
            // Solo : prix affiché, mais rien n'est débité (le solde reste à 0).
            Some(BuyItem::Weapon(i)) => {
                self.buy(i);
                self.buy_open = false;
                self.hold_fire = true;
            }
            // Les grenades s'ajoutent (le menu reste ouvert), dans les limites de CS.
            Some(BuyItem::Nade(n)) => {
                self.buy_nade(n);
                self.hold_fire = true;
            }
            Some(BuyItem::Gear(_)) | None => {}
        }
    }

    fn draw_buy(
        &self,
        ui: &mut Ui,
        size: (f32, f32),
        cursor: Option<(f32, f32)>,
        s: f32,
        col: &dyn Fn([u8; 3], f32) -> [f32; 4],
    ) {
        let (px, py, w, h, _, cw, rh) = Self::buy_layout(size);
        ui.rect(0.0, 0.0, size.0, size.1, [0.0, 0.0, 0.0, 0.35]);
        ui.rect(px, py, w, h, [0.02, 0.025, 0.03, 0.92]);
        ui.text(
            px + 8.0 * s,
            py + 6.0 * s,
            s * 1.5,
            col([240, 240, 240], 1.0),
            "Menu d'achat",
        );
        let solde = match self.online {
            Some(o) if o.warmup => "Échauffement : achats gratuits".to_string(),
            Some(o) if !o.can_buy => format!("Solde : {} $   (achats fermés)", o.money),
            Some(o) => format!("Solde : {} $", o.money),
            None => format!("Solde : {} $   (solo : achats gratuits)", self.money),
        };
        let tw = Ui::text_width(&solde, s);
        ui.text(
            px + w - tw - 8.0 * s,
            py + 9.0 * s,
            s,
            col([120, 220, 120], 1.0),
            solde,
        );
        let hover = cursor.and_then(|(x, y)| Self::buy_at(size, x, y));
        for (ci, cat) in Category::ALL.into_iter().enumerate() {
            let x = px + 8.0 * s + ci as f32 * cw;
            ui.text(x, py + 19.0 * s, s, col([200, 170, 90], 1.0), cat.label());
        }
        // Titre de l'équipement, sous les armes lourdes.
        if let Some((_, ci, k)) = Self::buy_grid()
            .into_iter()
            .find(|c| matches!(c.0, BuyItem::Nade(_)))
        {
            let x = px + 8.0 * s + ci as f32 * cw;
            let y = py + 28.0 * s + (k - 1) as f32 * rh;
            let t = format!("Équipement ({} / {})", self.nade_count(), equip::MAX_NADES);
            ui.text(x, y + 7.0 * s, s, col([200, 170, 90], 1.0), t);
        }
        for (item, ci, k) in Self::buy_grid() {
            let x = px + 8.0 * s + ci as f32 * cw;
            let y = py + 28.0 * s + k as f32 * rh;
            let (price, owned, enabled) = self.buy_state(item);
            let name = match item {
                BuyItem::Weapon(i) => WEAPONS[i].name.to_string(),
                BuyItem::Nade(n) => {
                    let have = self.nades[n.index()];
                    if have > 0 {
                        format!("{} x{}", n.name(), have)
                    } else {
                        n.name().to_string()
                    }
                }
                BuyItem::Gear(g) => GEAR
                    .iter()
                    .find(|(c, _)| *c == g)
                    .map_or("?", |(_, n)| n)
                    .to_string(),
            };
            let bg = if hover == Some(item) && enabled {
                col([70, 90, 120], 0.95)
            } else if owned {
                col([40, 70, 45], 0.9)
            } else {
                col([30, 34, 40], 0.9)
            };
            let (cwi, rhi) = (cw - 4.0 * s, rh - 2.0 * s);
            ui.rect(x, y, cwi, rhi, bg);
            // Modèle 3D de l'arme, de profil, derrière son nom.
            let tris: Vec<weapons::Tri> = match item {
                BuyItem::Weapon(i) => weapons::model(i).tris.clone(),
                BuyItem::Nade(n) => equip::nade_model(n),
                BuyItem::Gear(_) => Vec::new(),
            };
            ui.model(icon_3d(
                &tris,
                x + cwi * 0.62,
                y + rhi * 0.5,
                cwi * 0.7,
                rhi * 0.92,
            ));
            let tc = if enabled {
                [235, 235, 235]
            } else {
                [120, 120, 120]
            };
            ui.text(
                x + 3.0 * s,
                y + (rhi - 8.0 * s) / 2.0,
                s,
                col(tc, 1.0),
                name,
            );
            let p = format!("{price} $");
            let pw = Ui::text_width(&p, s);
            ui.text(
                x + cwi - 4.0 * s - pw,
                y + (rhi - 8.0 * s) / 2.0,
                s,
                col([150, 210, 150], 1.0),
                p,
            );
        }
        let info = match hover {
            Some(BuyItem::Weapon(i)) => {
                let d = &WEAPONS[i];
                format!(
                    "{} : {} dégâts, {} coups/min, chargeur {}{}",
                    d.name,
                    d.damage * d.pellets.max(1),
                    d.rpm as u32,
                    d.magazine,
                    if d.auto { ", automatique" } else { "" }
                )
            }
            Some(BuyItem::Nade(n)) => format!(
                "{} : {} au plus, 4 grenades en tout (règles de CS)",
                n.name(),
                n.max()
            ),
            Some(BuyItem::Gear(minelib_net::game::ITEM_KIT)) => {
                "Kit de désamorçage (CT) : désamorce en 5 s au lieu de 10.".to_string()
            }
            Some(BuyItem::Gear(minelib_net::game::ITEM_HELMET)) => {
                "Gilet + casque : protège le corps et la tête.".to_string()
            }
            Some(BuyItem::Gear(_)) => "Gilet : protège le corps (pas la tête).".to_string(),
            None => String::new(),
        };
        ui.text(
            px + 8.0 * s,
            py + h - 10.0 * s,
            s,
            col([200, 200, 210], 1.0),
            info,
        );
    }

    /// Entrées propres au menu d'achat ouvert (clic), à appeler avec le curseur libre.
    pub fn buy_input(&mut self, input: &Input, size: (f32, f32)) {
        if !self.buy_open {
            return;
        }
        if input.mouse_pressed(minelib_platform::Mouse::Left) {
            if let Some(c) = input.cursor {
                self.buy_click(size, c);
            }
        }
    }
}

/// Icône 3D d'un modèle pour l'interface : vu de trois quarts (canon vers la droite),
/// centré en (`cx`, `cy`) et tenant dans `w` × `h` pixels.
/// Assombrit une icône (objet pas en main).
fn dim(v: &mut [VmVertex], k: f32) {
    for vert in v {
        for c in 0..3 {
            vert.color[c] *= k;
        }
    }
}

fn icon_3d(tris: &[weapons::Tri], cx: f32, cy: f32, w: f32, h: f32) -> Vec<VmVertex> {
    let rot =
        Mat4::from_rotation_x(0.35) * Mat4::from_rotation_y(-std::f32::consts::FRAC_PI_2 + 0.45);
    let pts: Vec<Vec3> = tris
        .iter()
        .flat_map(|t| t.p)
        .map(|p| rot.transform_point3(p))
        .collect();
    if pts.is_empty() {
        return Vec::new();
    }
    let mn = pts.iter().fold(Vec3::splat(f32::MAX), |a, p| a.min(*p));
    let mx = pts.iter().fold(Vec3::splat(f32::MIN), |a, p| a.max(*p));
    let ext = (mx - mn).max(Vec3::splat(1e-4));
    let k = (w / ext.x).min(h / ext.y);
    let c = (mn + mx) * 0.5;
    let sun = Vec3::new(0.3, 0.9, 0.6).normalize();
    let mut out = Vec::with_capacity(tris.len() * 3);
    for t in tris {
        let p = t.p.map(|p| rot.transform_point3(p));
        let n = (p[1] - p[0]).cross(p[2] - p[0]).normalize_or_zero();
        let shade = 0.5 + 0.5 * n.dot(sun).abs();
        let col = t.color.map(|v| ((v * 2.2 + 0.02) * shade).min(1.0) * 0.8);
        for q in p {
            out.push(VmVertex {
                clip: [
                    cx + (q.x - c.x) * k,
                    cy - (q.y - c.y) * k,
                    // Profondeur (Z inversé) : le plus proche gagne.
                    0.5 + (q.z - c.z) / ext.z.max(1e-3) * 0.01,
                    1.0,
                ],
                uv: [0.0, 0.0],
                layer: NO_TEXTURE,
                _pad: 0,
                color: [col[0], col[1], col[2], 1.0],
            });
        }
    }
    out
}

/// Triangles d'un modèle placés par `m`, ombrés comme les armes.
fn draw_tris(out: &mut Vec<VmVertex>, proj: Mat4, m: Mat4, tris: &[weapons::Tri], light: [f32; 3]) {
    let mvp = proj * m;
    let sun = Vec3::new(0.3, 0.9, 0.4).normalize();
    for t in tris {
        let n = (t.p[1] - t.p[0]).cross(t.p[2] - t.p[0]).normalize_or_zero();
        let n = m.transform_vector3(n);
        let shade = 0.45 + 0.55 * n.dot(sun).abs();
        let c = t.color.map(|v| (v * 2.2 + 0.015) * shade);
        for p in t.p {
            out.push(VmVertex {
                clip: (mvp * p.extend(1.0)).to_array(),
                uv: [0.0, 0.0],
                layer: NO_TEXTURE,
                _pad: 0,
                color: [c[0] * light[0], c[1] * light[1], c[2] * light[2], 1.0],
            });
        }
    }
}

/// Direction du regard (lacet, tangage).
fn look_dir(yaw: f32, pitch: f32) -> Vec3 {
    Vec3::new(
        yaw.sin() * pitch.cos(),
        pitch.sin(),
        -yaw.cos() * pitch.cos(),
    )
}

fn push_tri(out: &mut Vec<VmVertex>, vp: Mat4, p: [Vec3; 3], color: [f32; 4]) {
    push_tri_c(out, vp, p, [color; 3]);
}

/// Triangle avec une couleur par sommet (dégradé).
fn push_tri_c(out: &mut Vec<VmVertex>, vp: Mat4, p: [Vec3; 3], colors: [[f32; 4]; 3]) {
    let clip = p.map(|q| vp * q.extend(1.0));
    // Derrière la caméra : on saute le triangle entier.
    if clip.iter().any(|c| c.w <= 0.01) {
        return;
    }
    for (c, color) in clip.iter().zip(colors) {
        out.push(VmVertex {
            clip: c.to_array(),
            uv: [0.0, 0.0],
            layer: NO_TEXTURE,
            _pad: 0,
            color,
        });
    }
}

fn quad(out: &mut Vec<VmVertex>, vp: Mat4, c: Vec3, u: Vec3, v: Vec3, color: [f32; 4]) {
    let (a, b, cc, d) = (c - u - v, c + u - v, c + u + v, c - u + v);
    push_tri(out, vp, [a, b, cc], color);
    push_tri(out, vp, [cc, d, a], color);
}

#[cfg(test)]
mod tests {
    use super::*;
    use minelib_world::{BlockId, ChunkPos};

    #[test]
    fn fall_damage_curve() {
        assert_eq!(Fps::fall_damage(4.0, 4.0), 0);
        assert!(Fps::fall_damage(6.0, 4.0) > 0);
        assert!(Fps::fall_damage(8.0, 4.0) < 25);
        assert!((24..=30).contains(&Fps::fall_damage(13.0, 4.0)));
        assert!(Fps::fall_damage(50.0, 4.0) < 100);
    }

    #[test]
    fn buying_click_held_does_not_fire() {
        let w = World::new(0);
        let mut f = Fps::new();
        let size = (1280.0, 720.0);
        let ak = WEAPONS.iter().position(|d| d.name == "AK-47").unwrap();
        // Trouve la case de l'AK-47 dans le menu et clique dessus.
        let mut spot = None;
        'search: for y in (0..720).step_by(4) {
            for x in (0..1280).step_by(4) {
                if Fps::buy_at(size, x as f32, y as f32) == Some(BuyItem::Weapon(ak)) {
                    spot = Some((x as f32, y as f32));
                    break 'search;
                }
            }
        }
        f.buy_open = true;
        f.buy_click(size, spot.unwrap());
        let held = Controls {
            trigger_held: true,
            trigger_down: true,
            on_ground: true,
            ..Default::default()
        };
        let eye = Vec3::new(0.5, 100.0, 0.5);
        for _ in 0..30 {
            f.update(1.0 / 60.0, &held, &w, eye, 0.0, 0.0);
        }
        assert_eq!(
            f.active().unwrap().mag,
            WEAPONS[ak].magazine,
            "tir avec le clic d'achat"
        );
        // Menu fermé mais souris pas encore reprise : le bouton toujours tenu (vu seulement
        // par `trigger_down`) ne libère pas le tir.
        let menu = Controls {
            trigger_down: true,
            on_ground: true,
            ..Default::default()
        };
        f.block_fire();
        f.update(1.0 / 60.0, &menu, &w, eye, 0.0, 0.0);
        for _ in 0..10 {
            f.update(1.0 / 60.0, &held, &w, eye, 0.0, 0.0);
        }
        assert_eq!(
            f.active().unwrap().mag,
            WEAPONS[ak].magazine,
            "tir après le menu"
        );
        // Relâché puis rappuyé : ça tire.
        f.update(1.0 / 60.0, &Controls::default(), &w, eye, 0.0, 0.0);
        let press = Controls {
            trigger_held: true,
            trigger_down: true,
            trigger_pressed: true,
            on_ground: true,
            ..Default::default()
        };
        f.deploy = 0.0; // arme sortie depuis longtemps
        f.update(1.0 / 60.0, &press, &w, eye, 0.0, 0.0);
        assert!(f.active().unwrap().mag < WEAPONS[ak].magazine);
    }

    #[test]
    fn switching_weapons_waits_before_firing() {
        let w = World::new(0);
        let eye = Vec3::new(0.5, 100.0, 0.5);
        let mut f = Fps::new();
        let ak = WEAPONS.iter().position(|d| d.name == "AK-47").unwrap();
        f.buy(ak);
        let fire = Controls {
            trigger_pressed: true,
            trigger_held: true,
            on_ground: true,
            ..Default::default()
        };
        // 0,7 s après la sortie : toujours rien.
        for _ in 0..42 {
            f.update(1.0 / 60.0, &fire, &w, eye, 0.0, 0.0);
        }
        assert_eq!(
            f.active().unwrap().mag,
            WEAPONS[ak].magazine,
            "tir trop tôt"
        );
        // Après 0,75 s : ça tire.
        for _ in 0..6 {
            f.update(1.0 / 60.0, &fire, &w, eye, 0.0, 0.0);
        }
        assert!(
            f.active().unwrap().mag < WEAPONS[ak].magazine,
            "toujours bloqué"
        );
    }

    #[test]
    fn knife_grenades_and_wheel_like_cs() {
        let mut w = World::new(0);
        w.insert_empty(minelib_world::ChunkPos { x: 0, z: 0 });
        for z in 0..16 {
            for x in 0..16 {
                w.set(x, 10, z, BlockId::STONE);
            }
        }
        let eye = Vec3::new(8.5, 12.62, 8.5);
        let mut f = Fps::new();
        // Chaque image de ce test arrive bien après la sortie de l'arme (délai ignoré).
        let tick = |f: &mut Fps, c: Controls| {
            f.deploy = 0.0;
            f.update(1.0 / 60.0, &c, &w, eye, 0.0, 0.0)
        };
        // Limites de CS : 2 flashs, une de chaque autre, 4 au total.
        assert!(f.buy_nade(Nade::Flash) && f.buy_nade(Nade::Flash));
        assert!(!f.buy_nade(Nade::Flash));
        assert!(f.buy_nade(Nade::He) && f.buy_nade(Nade::Smoke));
        assert!(!f.buy_nade(Nade::Molotov), "4 grenades au plus");
        f.buy(WEAPONS.iter().position(|d| d.name == "AK-47").unwrap());
        // Molette : principale → pistolet → couteau → HE → flash → fumigène → principale.
        let wheel = |f: &mut Fps| {
            tick(
                f,
                Controls {
                    wheel: 1,
                    ..Default::default()
                },
            )
        };
        let mut seen = vec![f.sel];
        for _ in 0..6 {
            wheel(&mut f);
            seen.push(f.sel);
        }
        assert_eq!(
            seen,
            vec![
                Sel::Primary,
                Sel::Secondary,
                Sel::Knife,
                Sel::Nade(Nade::He),
                Sel::Nade(Nade::Flash),
                Sel::Nade(Nade::Smoke),
                Sel::Primary
            ]
        );
        // Lancer la HE : elle part, explose, et le joueur tout près est blessé.
        tick(
            &mut f,
            Controls {
                nade_key: Some(Nade::He),
                ..Default::default()
            },
        );
        tick(
            &mut f,
            Controls {
                trigger_pressed: true,
                trigger_held: true,
                ..Default::default()
            },
        );
        assert_eq!(f.nades[Nade::He.index()], 0);
        assert_ne!(f.sel, Sel::Nade(Nade::He), "plus de HE en main");
        // Couteau : marque le bloc visé.
        tick(
            &mut f,
            Controls {
                slot: Some(2),
                ..Default::default()
            },
        );
        assert_eq!(f.sel, Sel::Knife);
        let holes = f.holes.len();
        f.deploy = 0.0;
        f.update(
            1.0 / 60.0,
            &Controls {
                trigger_held: true,
                trigger_pressed: true,
                ..Default::default()
            },
            &w,
            eye,
            0.0,
            -1.4,
        );
        assert_eq!(f.holes.len(), holes + 1, "coup de couteau sur le sol");
    }

    #[test]
    fn r8_fires_only_after_priming() {
        let w = World::new(0);
        let eye = Vec3::new(0.5, 100.0, 0.5);
        let mut f = Fps::new();
        let r8 = WEAPONS
            .iter()
            .position(|d| d.name == "R8 Revolver")
            .unwrap();
        f.buy(r8);
        let hold = Controls {
            trigger_held: true,
            trigger_down: true,
            trigger_pressed: true,
            ..Default::default()
        };
        let mut fired_at = None;
        for i in 0..60 {
            f.update(1.0 / 60.0, &hold, &w, eye, 0.0, 0.0);
            if fired_at.is_none() && f.active().unwrap().mag < WEAPONS[r8].magazine {
                fired_at = Some(i);
            }
        }
        let t = fired_at.expect("le R8 doit tirer") as f32 / 60.0;
        assert!((0.35..0.5).contains(&t), "coup parti après {t} s");
    }

    #[test]
    fn running_widens_the_cone_with_speed() {
        let ak = &WEAPONS[WEAPONS.iter().position(|d| d.name == "AK-47").unwrap()];
        let run = crate::player::FPS_SPRINT;
        let still = spread_deg(ak, 0.0, true, false, false);
        assert_eq!(
            spread_deg(ak, run * 0.3, true, false, false),
            still,
            "lent : précis"
        );
        let walk = spread_deg(ak, crate::player::FPS_WALK, true, false, false);
        let running = spread_deg(ak, run, true, false, false);
        let hopping = spread_deg(ak, run * 1.5, true, false, false);
        assert!(still < walk && walk < running && running < hopping);
        assert!(running > 5.0, "{still} → {running}");
        assert!(spread_deg(ak, run, false, false, false) > running * 2.0);
        // À l'arrêt : droit, sauf arme à lunette sans lunette.
        assert_eq!(still, 0.0);
        let awp = &WEAPONS[WEAPONS.iter().position(|d| d.name == "AWP").unwrap()];
        assert!(spread_deg(awp, 0.0, true, false, false) > 1.0);
        let settled = spread_deg(awp, 0.0, true, false, true);
        assert!((settled - spread_deg(awp, 0.0, true, false, false) * 0.4).abs() < 1e-5);
        assert_eq!(spread_deg(awp, 0.0, true, true, false), 0.0);
        let nova = WEAPONS.iter().find(|d| d.pellets > 1).unwrap();
        assert!(
            spread_deg(nova, 0.0, true, false, false) > 0.0,
            "plombs dispersés"
        );
    }

    #[test]
    fn scopes_cycle_like_cs_and_weapons_switch() {
        let w = World::new(0);
        let eye = Vec3::new(0.5, 100.0, 0.5);
        let mut f = Fps::new();
        let press = |f: &mut Fps, c: Controls| {
            f.deploy = 0.0;
            f.update(1.0 / 60.0, &c, &w, eye, 0.0, 0.0)
        };
        let scope = || Controls {
            scope: true,
            on_ground: true,
            ..Default::default()
        };
        for (name, mags) in [
            ("AWP", &[2.0, 4.0][..]),
            ("SSG 08", &[2.0, 4.0][..]),
            ("AUG", &[2.0][..]),
            ("SG 553", &[2.0][..]),
        ] {
            f.buy(WEAPONS.iter().position(|d| d.name == name).unwrap());
            assert_eq!(f.scope_mag(), None);
            for &m in mags {
                press(&mut f, scope());
                assert_eq!(f.scope_mag(), Some(m), "{name}");
            }
            // Nouveau clic : sans lunette.
            press(&mut f, scope());
            assert_eq!(f.scope_mag(), None, "{name}");
        }
        // ×2 : champ de vision 2 fois plus étroit (en tangente).
        press(&mut f, scope());
        let fov = f.scope_fov().unwrap();
        let ratio = (crate::session::FOV.to_radians() / 2.0).tan() / (fov.to_radians() / 2.0).tan();
        assert!((ratio - 2.0).abs() < 1e-3);
        // Arme précédente, lâcher, racheter.
        let q = || Controls {
            last_weapon: true,
            ..Default::default()
        };
        assert!(f.use_primary);
        press(&mut f, q());
        assert!(!f.use_primary);
        press(&mut f, q());
        assert!(f.use_primary);
        press(
            &mut f,
            Controls {
                drop: true,
                ..Default::default()
            },
        );
        assert!(f.primary.is_none() && !f.use_primary);
        press(
            &mut f,
            Controls {
                rebuy: true,
                ..Default::default()
            },
        );
        assert_eq!(f.active().map(|g| WEAPONS[g.def].name), Some("SG 553"));
    }

    #[test]
    fn shooting_uses_ammo_makes_holes_and_recoils() {
        let mut w = World::new(0);
        w.insert_empty(ChunkPos { x: 0, z: 0 });
        for y in 0..16 {
            for x in 0..16 {
                w.set(x, y, 4, BlockId::STONE); // mur au nord (z = 4)
            }
        }
        let mut f = Fps::new();
        let ak = WEAPONS.iter().position(|d| d.name == "AK-47").unwrap();
        f.buy(ak);
        assert_eq!(f.money, 0, "solo : achat gratuit, solde inchangé");
        let eye = Vec3::new(8.5, 8.0, 12.5);
        let c = Controls {
            trigger_pressed: true,
            trigger_held: true,
            on_ground: true,
            ..Default::default()
        };
        for _ in 0..120 {
            f.update(1.0 / 60.0, &c, &w, eye, 0.0, 0.0);
            f.tick_effects(1.0 / 60.0);
        }
        let g = f.active().unwrap();
        assert!(g.mag < 30, "des balles tirées");
        assert!(!f.holes.is_empty(), "impacts sur le mur");
        assert!(f.punch.0 > 0.5, "la vue remonte");
        // Relâché : le motif et la vue reviennent.
        let idle = Controls {
            on_ground: true,
            ..Default::default()
        };
        for _ in 0..120 {
            f.update(1.0 / 60.0, &idle, &w, eye, 0.0, 0.0);
        }
        assert_eq!(f.spray_n, 0);
        assert!(f.punch.0 < 0.05);
        // Rechargement.
        let r = Controls {
            reload: true,
            on_ground: true,
            ..Default::default()
        };
        f.update(1.0 / 60.0, &r, &w, eye, 0.0, 0.0);
        for _ in 0..200 {
            f.update(1.0 / 60.0, &idle, &w, eye, 0.0, 0.0);
        }
        assert_eq!(f.active().unwrap().mag, 30);
    }
}
