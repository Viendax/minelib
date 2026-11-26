//! Messages de jeu entre les joueurs et l'hôte d'une partie (transportés par le relais
//! dans `Request::ToHost` / `Response::FromHost`, sérialisés en postcard).
//!
//! L'hôte fait foi : chaque joueur lui envoie ses entrées, pas sa position. Il simule tout
//! le monde au même pas fixe (128/s), puis renvoie l'état ; le joueur prédit son propre
//! déplacement et se corrige si l'hôte n'est pas d'accord.

use serde::{Deserialize, Serialize};

/// Entrées d'un pas de simulation.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq)]
pub struct NetInput {
    pub forward: i8,
    pub strafe: i8,
    pub jump: bool,
    pub jump_pressed: bool,
    /// Marcher (Maj) : plus lent, silencieux.
    pub walk: bool,
    pub duck: bool,
    pub yaw: f32,
    pub pitch: f32,
    /// Vitesse permise par ce qu'il tient (unités de CS : 250 couteau, 200 AWP…).
    pub carry: u8,
}

/// État complet du déplacement d'un joueur (de quoi reprendre la simulation à l'identique).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq)]
pub struct SimState {
    pub pos: [f32; 3],
    pub vel: [f32; 3],
    pub on_ground: bool,
    pub crouch: f32,
    pub stamina: f32,
    pub duck_speed: f32,
    pub jump_buffer: f32,
    pub sneak_held: bool,
    /// Vitesse permise par ce qu'il tient (unités de CS).
    pub carry: u8,
}

/// Position d'un joueur à un pas (message court) : regard en 65536e de tour, accroupi
/// et vitesse sur un octet.
/// (v334) Position relative à celle du joueur qui reçoit (`you.pos` du même message), en
/// `POSE_UNIT`es de bloc : 4 à 6 octets au lieu de 12 pour des joueurs à quelques dizaines
/// de blocs, sans limite de distance ; cap sur 2 octets fixes (3 avant, le plus souvent).
/// (v337) Regard en 4096e de tour (0,09° : pour voir où les autres regardent ; leurs
/// coups, l'hôte les compte avec le regard exact) : de petits écarts d'un message à
/// l'autre, pour les différences.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub id: u64,
    pub rel: [i32; 3],
    #[serde(with = "fixed16")]
    pub yaw: i16,
    pub pitch: i16,
    pub crouch: u8,
    pub speed: u8,
}

/// (v334) Finesse des positions de `Pose` : un 64e de bloc (moins d'un centimètre d'erreur).
pub const POSE_UNIT: f32 = 64.0;

/// (v337) Un tour, en unités du regard de `Pose`.
pub const POSE_TURN: i64 = 4096;

impl Pose {
    /// `anchor` : la position du joueur qui reçoit, telle qu'elle part dans le même message.
    pub fn of(p: &NetPlayer, anchor: [f32; 3]) -> Self {
        let turn = |a: f32| {
            let t = (a / std::f32::consts::TAU * POSE_TURN as f32).round() as i64;
            ((t + POSE_TURN / 2).rem_euclid(POSE_TURN) - POSE_TURN / 2) as i16
        };
        let q = |v: f32, a: f32| {
            ((f64::from(v) - f64::from(a)) * f64::from(POSE_UNIT))
                .round()
                .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
        };
        Pose {
            id: p.id,
            rel: [
                q(p.pos[0], anchor[0]),
                q(p.pos[1], anchor[1]),
                q(p.pos[2], anchor[2]),
            ],
            yaw: turn(p.yaw),
            pitch: turn(p.pitch),
            crouch: (p.crouch.clamp(0.0, 1.0) * 255.0) as u8,
            speed: (p.speed * 10.0).clamp(0.0, 255.0) as u8,
        }
    }

    /// Met `p` à cette position (`anchor` : la même que pour `of`).
    pub fn apply(&self, p: &mut NetPlayer, anchor: [f32; 3]) {
        let angle = |a: i16| a as f32 / POSE_TURN as f32 * std::f32::consts::TAU;
        let back = |r: i32, a: f32| (f64::from(a) + f64::from(r) / f64::from(POSE_UNIT)) as f32;
        p.pos = [
            back(self.rel[0], anchor[0]),
            back(self.rel[1], anchor[1]),
            back(self.rel[2], anchor[2]),
        ];
        p.yaw = angle(self.yaw);
        p.pitch = angle(self.pitch);
        p.crouch = self.crouch as f32 / 255.0;
        p.speed = self.speed as f32 / 10.0;
    }
}

/// (v334) Un entier de 16 bits sur 2 octets fixes (un cap tiré au hasard en prend 3 en
/// varint).
mod fixed16 {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    pub fn serialize<S: Serializer>(v: &i16, s: S) -> Result<S::Ok, S::Error> {
        v.to_le_bytes().serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<i16, D::Error> {
        Ok(i16::from_le_bytes(<[u8; 2]>::deserialize(d)?))
    }
}

/// Un joueur vu par les autres.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct NetPlayer {
    pub id: u64,
    /// (v333) Vide sur le fil quand celui qui reçoit l'a déjà (voir `Host::snapshots`).
    pub pseudo: String,
    pub pos: [f32; 3],
    /// (v333) Sur le fil comme dans `Pose` : les angles en 65536e de tour, l'accroupi et la
    /// vitesse sur un octet (16 octets de moins par joueur, 64 fois par seconde).
    /// (v334) Le cap sur 2 octets fixes.
    #[serde(with = "turn16fixed")]
    pub yaw: f32,
    #[serde(with = "turn16")]
    pub pitch: f32,
    #[serde(with = "unit8")]
    pub crouch: f32,
    /// Vitesse au sol (animation de marche).
    #[serde(with = "tenth8")]
    pub speed: f32,
    pub hp: u8,
    pub alive: bool,
    /// Arme en main (indice dans la liste des armes, 255 = couteau ou rien ; (v177)
    /// grenade : `ITEM_NADE` + type, bombe : 252).
    pub weapon: u8,
    pub kills: u16,
    pub deaths: u16,
    /// Équipe (`CT` ou `T`).
    pub team: u8,
    /// `FLAG_BOMB`, `FLAG_KIT`, `FLAG_DEFUSING`, `FLAG_PLANTING`.
    pub flags: u8,
}

/// (v333) Un angle (radians) en 65536e de tour sur le fil, comme dans `Pose`.
mod turn16 {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    pub fn serialize<S: Serializer>(a: &f32, s: S) -> Result<S::Ok, S::Error> {
        ((*a / std::f32::consts::TAU * 65536.0).round() as i64 as i16).serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<f32, D::Error> {
        Ok(i16::deserialize(d)? as f32 / 65536.0 * std::f32::consts::TAU)
    }
}

/// (v334) Le même, sur 2 octets fixes (pour le cap, tiré sur tout le tour).
mod turn16fixed {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    pub fn serialize<S: Serializer>(a: &f32, s: S) -> Result<S::Ok, S::Error> {
        ((*a / std::f32::consts::TAU * 65536.0).round() as i64 as i16)
            .to_le_bytes()
            .serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<f32, D::Error> {
        Ok(i16::from_le_bytes(<[u8; 2]>::deserialize(d)?) as f32 / 65536.0 * std::f32::consts::TAU)
    }
}

/// (v333) Une valeur de 0 à 1 sur un octet.
mod unit8 {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    pub fn serialize<S: Serializer>(v: &f32, s: S) -> Result<S::Ok, S::Error> {
        ((v.clamp(0.0, 1.0) * 255.0).round() as u8).serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<f32, D::Error> {
        Ok(u8::deserialize(d)? as f32 / 255.0)
    }
}

/// (v333) Une vitesse au dixième, sur un octet (25,5 au plus).
mod tenth8 {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    pub fn serialize<S: Serializer>(v: &f32, s: S) -> Result<S::Ok, S::Error> {
        ((v * 10.0).clamp(0.0, 255.0).round() as u8).serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<f32, D::Error> {
        Ok(u8::deserialize(d)? as f32 / 10.0)
    }
}

pub const CT: u8 = 0;
pub const T: u8 = 1;
pub const FLAG_BOMB: u8 = 1;
pub const FLAG_KIT: u8 = 2;
pub const FLAG_DEFUSING: u8 = 4;
pub const FLAG_PLANTING: u8 = 8;

/// Codes d'achat : armes 0..100 (indice dans la liste des armes), grenades 100 + n,
/// équipement 200 et plus.
pub const ITEM_NADE: u16 = 100;
/// Code de `ToHost::Drop` pour lâcher la bombe.
pub const DROP_BOMB: u8 = 110;
pub const ITEM_KIT: u16 = 200;
pub const ITEM_KEVLAR: u16 = 201;
pub const ITEM_HELMET: u16 = 202;

/// Phase d'une partie compétitive.
pub mod phase {
    /// Échauffement : on attend 10 joueurs (réapparitions, argent illimité).
    pub const WARMUP: u8 = 0;
    /// Début de manche : immobiles, achats.
    pub const FREEZE: u8 = 1;
    pub const LIVE: u8 = 2;
    /// Bombe posée.
    pub const PLANTED: u8 = 3;
    pub const ROUND_END: u8 = 4;
    /// Mi-temps : changement de côté.
    pub const HALFTIME: u8 = 5;
    pub const GAME_OVER: u8 = 6;
}

/// Raisons de fin de manche.
pub mod reason {
    pub const T_ELIMINATED: u8 = 0;
    pub const CT_ELIMINATED: u8 = 1;
    pub const TIME: u8 = 2;
    pub const EXPLODED: u8 = 3;
    pub const DEFUSED: u8 = 4;
}

/// État de la bombe.
pub mod bomb {
    pub const NONE: u8 = 0;
    pub const CARRIED: u8 = 1;
    pub const DROPPED: u8 = 2;
    pub const PLANTED: u8 = 3;
    pub const DEFUSED: u8 = 4;
    pub const EXPLODED: u8 = 5;
}

/// Ce que le HUD du joueur montre (envoyé avec chaque état).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq)]
pub struct MatchHud {
    pub phase: u8,
    /// Temps restant de la phase (s) ; bombe posée : temps avant l'explosion.
    pub time: f32,
    /// Numéro de la manche (1…), 0 pendant l'échauffement.
    pub round: u8,
    /// Manches gagnées par les CT et les T (côtés actuels).
    pub score: [u8; 2],
    pub team: u8,
    pub money: u32,
    pub armor: u8,
    pub helmet: bool,
    pub kit: bool,
    pub bomb: u8,
    /// C'est ce joueur qui porte la bombe.
    pub carrying: bool,
    pub bomb_pos: [f32; 3],
    /// Pose ou désamorçage en cours (0..1), 0 sinon.
    pub progress: f32,
    /// Achats encore permis (temps d'achat et zone de départ).
    pub can_buy: bool,
    pub alive: [u8; 2],
    /// Site où la bombe est posée (0 = A).
    pub site: u8,
    /// Joueurs présents / nécessaires pendant l'échauffement.
    pub players: u8,
    /// Pause en cours : sorte (`PAUSE_…`, 0 aucune), temps restant (s).
    pub pause: u8,
    pub pause_left: f32,
    /// Pauses tactiques qui restent à son équipe.
    pub timeouts: u8,
    /// Vote de son équipe en cours.
    pub vote: Option<VoteHud>,
}

/// Sortes de votes (comme dans CS) : exclure un coéquipier, pause tactique.
pub const VOTE_KICK: u8 = 1;
pub const VOTE_PAUSE: u8 = 2;
/// Abandonner la partie (toute l'équipe doit être d'accord) : l'autre équipe gagne.
pub const VOTE_SURRENDER: u8 = 3;

/// Sortes de pauses : tactique (30 s, demandée par une équipe), technique (joueur
/// déconnecté : jusqu'à son retour, 5 minutes au plus).
pub const PAUSE_TACTICAL: u8 = 1;
pub const PAUSE_TECHNICAL: u8 = 2;

/// Vote en cours, tel que le voit un joueur de l'équipe (joueurs : identifiants, les
/// pseudos sont dans le tableau des scores).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct VoteHud {
    pub kind: u8,
    pub caller: u64,
    /// Joueur visé (exclusion).
    pub target: u64,
    pub yes: u8,
    pub no: u8,
    /// Oui nécessaires.
    pub need: u8,
    pub left: f32,
    /// Ce joueur peut voter (pas encore voté, pas visé).
    pub can_vote: bool,
}

/// Une ligne du tableau des scores (touche Tab).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ScoreRow {
    pub id: u64,
    pub pseudo: String,
    pub team: u8,
    pub kills: u16,
    pub deaths: u16,
    pub assists: u16,
    pub mvps: u16,
    pub headshots: u16,
    pub damage: u32,
    pub score: i32,
    pub alive: bool,
    pub money: u32,
    pub elo: i32,
    /// Couleur du joueur dans son équipe (0 à 4).
    pub color: u8,
    /// (v165) Ping du joueur avec l'hôte de la partie (ms ; 0 : l'hôte lui-même).
    pub ping: u16,
}

/// Résultat d'une partie, envoyé au relais par l'hôte et confirmé par les joueurs.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct MatchSummary {
    pub map_sig: [u8; 32],
    /// Manches gagnées par l'équipe qui a commencé CT, et par celle qui a commencé T.
    pub score: [u8; 2],
    pub rounds: u8,
    pub players: Vec<SummaryPlayer>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SummaryPlayer {
    pub pseudo: String,
    /// Équipe de départ.
    pub start_team: u8,
    pub kills: u16,
    pub deaths: u16,
    pub assists: u16,
    pub headshots: u16,
    pub damage: u32,
    pub mvps: u16,
    pub score: i32,
    pub rounds: u8,
    /// Coéquipiers tués (le facteur de confiance baisse un peu à chacun).
    pub teamkills: u16,
    /// Exclu par un vote de son équipe (pas de pénalité d'absence en plus).
    #[serde(default)]
    pub kicked: bool,
}

/// Une boîte de blocs (coins inclus).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Zone {
    pub min: [i32; 3],
    pub max: [i32; 3],
}

impl Zone {
    pub fn new(a: [i32; 3], b: [i32; 3]) -> Self {
        Self {
            min: [a[0].min(b[0]), a[1].min(b[1]), a[2].min(b[2])],
            max: [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])],
        }
    }

    /// Le point (en blocs, pas entiers) est dans la boîte.
    pub fn contains(&self, p: [f32; 3]) -> bool {
        (0..3).all(|i| p[i] >= self.min[i] as f32 && p[i] < self.max[i] as f32 + 1.0)
    }

    /// (v172) Un joueur dont les pieds sont en `p` s'y tient : dans la boîte, ou debout sur
    /// sa couche du dessus (une zone sélectionnée au sol à la baguette, ce sont les blocs du
    /// sol : on se tient un bloc plus haut).
    pub fn holds(&self, p: [f32; 3]) -> bool {
        p[0] >= self.min[0] as f32
            && p[0] < self.max[0] as f32 + 1.0
            && p[2] >= self.min[2] as f32
            && p[2] < self.max[2] as f32 + 1.0
            && p[1] >= self.min[1] as f32
            && p[1] < self.max[1] as f32 + 2.0
    }

    pub fn center(&self) -> [f32; 3] {
        [
            (self.min[0] + self.max[0] + 1) as f32 / 2.0,
            self.min[1] as f32,
            (self.min[2] + self.max[2] + 1) as f32 / 2.0,
        ]
    }
}

/// Réglages FPS d'une carte (`/fps set …`) : zones de départ et sites de bombe.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct FpsMap {
    pub ct: Option<Zone>,
    pub t: Option<Zone>,
    pub sites: [Option<Zone>; 3],
    /// Nombre de joueurs (pair, 2 à 20 ; 0 : 10). Seule la 5 contre 5 est classée.
    /// Bit `DOORS` : portes et trappes utilisables en partie (`/fps set door true`) ; il
    /// est rangé ici pour que les cartes déjà partagées se lisent toujours.
    pub max_players: u8,
    /// (v258) Portes posées pour la partie (`/fps set door`, à la baguette ou en visant) : une
    /// boîte par porte (ses deux blocs) ou double porte (quatre). Hors de l'encodage de la
    /// carte (les cartes déjà partagées se lisent toujours) : elles voyagent dans les lignes
    /// du paquet (`porte,…`, comme les lumières de l'éditeur), et dans `fps.txt`.
    #[serde(skip)]
    pub door_list: Vec<Zone>,
}

impl FpsMap {
    /// Bit de `max_players` : portes utilisables en partie.
    pub const DOORS: u8 = 0x80;
    /// Bit de `max_players` : une double porte s'ouvre d'un coup (`/fps set doorsync true`).
    pub const DOOR_SYNC: u8 = 0x40;
    const FLAGS: u8 = Self::DOORS | Self::DOOR_SYNC;

    /// Joueurs d'une partie sur cette carte.
    pub fn players(&self) -> u8 {
        match self.max_players & !Self::FLAGS {
            n @ 2..=20 => n & !1,
            _ => 10,
        }
    }

    /// Nombre de joueurs choisi (0 : par défaut).
    pub fn chosen_players(&self) -> u8 {
        self.max_players & !Self::FLAGS
    }

    pub fn set_players(&mut self, n: u8) {
        self.max_players = (self.max_players & Self::FLAGS) | (n & !Self::FLAGS);
    }

    /// Portes et trappes s'ouvrent et se ferment pendant la partie.
    pub fn doors(&self) -> bool {
        self.max_players & Self::DOORS != 0
    }

    /// Les deux battants d'une double porte s'ouvrent et se ferment ensemble.
    pub fn door_sync(&self) -> bool {
        self.max_players & Self::DOOR_SYNC != 0
    }

    pub fn set_door_sync(&mut self, on: bool) {
        if on {
            self.max_players |= Self::DOOR_SYNC;
        } else {
            self.max_players &= !Self::DOOR_SYNC;
        }
    }

    pub fn set_doors(&mut self, on: bool) {
        if on {
            self.max_players |= Self::DOORS;
        } else {
            self.max_players &= !Self::DOORS;
        }
    }

    /// (v258) Le bloc `p` fait partie d'une porte posée pour la partie (`door_list`).
    pub fn door_listed(&self, p: [i32; 3]) -> bool {
        self.door_list
            .iter()
            .any(|z| (0..3).all(|k| p[k] >= z.min[k] && p[k] <= z.max[k]))
    }

    /// (v258) La porte (ou la trappe) en `p` s'ouvre en partie : toutes le font
    /// (`/fps set door on`), ou elle a été posée pour la partie.
    pub fn door_usable(&self, p: [i32; 3]) -> bool {
        self.doors() || self.door_listed(p)
    }
}

/// Une carte complète, telle qu'on la partage : sa signature est l'empreinte de ces
/// octets (le nom n'en fait pas partie : deux copies d'une même carte sont la même carte).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct MapPackage {
    pub seed: u32,
    pub generator: String,
    /// Blocs modifiés : enregistrements de 14 octets triés.
    pub edits: Vec<u8>,
    /// Chunks importés : (x, z, données), triés.
    pub chunks: Vec<(i32, i32, Vec<u8>)>,
    /// Torches (lignes triées).
    pub torches: Vec<String>,
    pub fps: FpsMap,
}

impl MapPackage {
    pub fn to_bytes(&self) -> Vec<u8> {
        encode(self)
    }

    pub fn signature(&self) -> [u8; 32] {
        signature(&self.to_bytes())
    }
}

/// Signature d'une carte à partir de ses octets.
pub fn signature(bytes: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"minelib/carte/v2");
    h.update(bytes);
    h.finalize().into()
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum ToHost {
    /// Arrivée dans la partie, avec l'empreinte de la carte qu'on a.
    Hello {
        map_hash: [u8; 32],
    },
    /// Entrées des pas `first_seq`, `first_seq + 1`… (dans l'ordre).
    Inputs {
        first_seq: u32,
        inputs: Vec<NetInput>,
        weapon: u8,
    },
    /// Tir : origine, direction de chaque plomb (dispersion comprise) et temps de
    /// l'affichage du tireur (pour rembobiner les cibles comme il les voyait).
    Fire {
        weapon: u8,
        origin: [f32; 3],
        dirs: Vec<[f32; 3]>,
        view_tick: u32,
        /// Le tireur est aveuglé par une flash (affiché au fil des éliminations).
        blind: bool,
    },
    Chat(String),
    /// Achat (code `ITEM_…` ou indice d'arme).
    Buy(u16),
    /// Touche de pose (bombe en main, tir maintenu) / de désamorçage (touche utiliser).
    Action {
        plant: bool,
        defuse: bool,
    },
    /// Le joueur choisit son équipe (échauffement seulement).
    Team(u8),
    /// Grenade lancée (tout le monde la simule : fumée, flash, feu au même endroit).
    Throw {
        nade: u8,
        pos: [f32; 3],
        vel: [f32; 3],
    },
    /// Armes portées (indices, 255 = aucune).
    Loadout {
        primary: u8,
        secondary: u8,
    },
    /// Arme, grenade ou bombe (`DROP_BOMB`) lâchée (touche G), lancée en cloche :
    /// `at` est le point où elle retombe (calculé par le joueur, vérifié par l'hôte).
    Drop {
        weapon: u8,
        at: [f32; 3],
    },
    /// Ramasser une arme au sol.
    Pickup {
        id: u32,
    },
    /// Couleur préférée (0 à 9 ; 255 : aucune).
    Color(u8),
    /// Ouvre ou ferme la porte / trappe en `pos` (si la carte le permet). `side` : côté
    /// vers lequel regarde le joueur (0 +X, 1 −X, 4 +Z, 5 −Z), pour relever une trappe.
    Door {
        pos: [i32; 3],
        side: u8,
    },
    /// Propose un vote à son équipe (`VOTE_…`) ; `target` : joueur à exclure.
    CallVote {
        kind: u8,
        target: u64,
    },
    /// Réponse au vote de son équipe (F1 oui, F2 non).
    Ballot(bool),
    /// (v165) Son ping avec l'hôte (ms), mesuré par ses entrées confirmées : montré à tous
    /// dans le tableau des scores.
    Ping(u16),
    /// (v177) Revente d'un achat de cette manche (code d'article comme `Buy`).
    Sell(u16),
    /// Les derniers pas d'entrées, envoyés aussi par la voie rapide (`crate::fast`) : ils
    /// sont déjà partis par le relais dans `Inputs`. L'hôte n'en prend que le pas qui suit
    /// exactement le dernier qu'il a : jamais de trou, ce qui manque arrive par le relais.
    InputsFast {
        first_seq: u32,
        inputs: Vec<NetInput>,
        weapon: u8,
    },
    /// (v333) Le lien de ce joueur avec l'hôte ne suit pas 256 positions par seconde (pas de
    /// voie rapide, et le relais prend du retard) : une mise à jour de positions tous les
    /// `every` pas seulement (1 : 256 par seconde ; 2 : 128 ; 4 : plus que les états
    /// complets, 64).
    Rate {
        every: u8,
    },
    /// (v337) Une différence est arrivée sans le message d'avant sur lequel elle s'appuie :
    /// les prochains partent entiers.
    DeltaMiss,
    /// (v337) Les mêmes que `Inputs`, serrées (`pack_inputs`) : chaque pas d'entrées par
    /// différence avec le précédent (le plus souvent un ou deux octets au lieu de 15).
    InputsPacked {
        first_seq: u32,
        data: Vec<u8>,
        weapon: u8,
    },
    /// (v338) Les mêmes que `InputsFast`, serrées, avec le pas des derniers positions et
    /// état complet que le joueur a (`moves`, `snap`) : par la voie rapide, qui perd des
    /// paquets, l'hôte n'écrit ses différences qu'à partir de messages ainsi confirmés.
    InputsFastPacked {
        first_seq: u32,
        data: Vec<u8>,
        weapon: u8,
        moves: u32,
        snap: u32,
    },
}

/// Messages d'un monde de construction en ligne (passent par les zones du relais).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum BuildMsg {
    /// Demande d'un joueur au relais du monde : mettre `block` en `pos`. `fluid` : c'est
    /// l'écoulement d'un fluide qui change cette case (budget à part, pas de minage).
    Edit {
        pos: [i32; 3],
        block: u16,
        fluid: bool,
    },
    /// Modification acceptée par le relais du monde (gardée, compactée : la dernière par
    /// case). `by` : son auteur (le pseudo de sa session) ; `seq` : son rang dans l'ordre
    /// du monde. Le jeu n'applique jamais une modification plus ancienne que celle qu'il a
    /// déjà pour la même case : un doublon, ou la même arrivée par deux chemins, est sans
    /// effet.
    Set {
        pos: [i32; 3],
        block: u16,
        by: String,
        seq: u64,
    },
    /// (v318) Ses pieds, en double précision, sur la carte bouclée.
    Player {
        pseudo: String,
        pos: [f64; 3],
        yaw: f32,
        pitch: f32,
        crouch: f32,
        speed: f32,
        /// (v165) Son ping avec son relais (ms), montré dans la liste des joueurs (Tab).
        ping: u16,
        /// (v222) Ce qu'il tient en main (`poses::Held::byte` : lampe torche, Foudre de
        /// Zeus), 0 : rien de cela. Les autres le voient dans sa main (la lumière de la
        /// lampe allumée, le crépitement de l'éclair).
        held: u8,
    },
    /// Le relais du monde refuse la modification du joueur `peer` en `pos` : remise à lui
    /// seul, qui remet le bloc d'avant.
    Reject { pos: [i32; 3], peer: u64 },
    // ---- Objets au sol et coffres (voir `items`) : demandes à l'arbitre…
    /// Lâcher `pile` (déjà sortie de l'inventaire) en `pos`, sous le numéro neuf `id`.
    DropItem {
        seq: u32,
        id: u64,
        pos: [f64; 3],
        pile: crate::items::Pile,
    },
    /// Ramasser l'objet `id`.
    TakeItem { seq: u32, id: u64 },
    /// Poser `pile` (déjà sortie de l'inventaire) dans l'emplacement d'un coffre.
    ChestPut {
        seq: u32,
        pos: [i32; 3],
        slot: u8,
        pile: crate::items::Pile,
    },
    /// Prendre `count` objets d'un emplacement de coffre.
    ChestTake {
        seq: u32,
        pos: [i32; 3],
        slot: u8,
        count: u8,
    },
    // … et états publiés par l'arbitre.
    /// Objet au sol `id` : présent (`drop`) ou disparu. `to`, `seq` : le joueur dont la
    /// demande vient d'être acceptée ; `got` : ce qu'il gagne (objet ramassé).
    Item {
        id: u64,
        at: [f64; 3],
        drop: Option<crate::items::Pile>,
        to: String,
        seq: u32,
        got: Option<crate::items::Pile>,
    },
    /// Emplacement d'un coffre après la demande `seq` de `by` ; `got` : ce qu'il gagne.
    Slot {
        pos: [i32; 3],
        slot: u8,
        pile: Option<crate::items::Pile>,
        by: String,
        seq: u32,
        got: Option<crate::items::Pile>,
    },
    /// Demande `seq` refusée (remise au seul demandeur) : rien n'a changé.
    Denied { seq: u32 },
    /// Blocs changés, regroupés (voir `crate::cells`) : ce que disent autant de `Set`,
    /// sans l'auteur, en six à huit octets par bloc au lieu d'une cinquantaine.
    Cells(Vec<u8>),
    /// Positions de joueurs, regroupées (voir `crate::poses`) : ce que disent autant de
    /// `Player`, en une dizaine d'octets par joueur au lieu d'une soixantaine.
    Poses(Vec<u8>),
    /// Région souveraine : demande de modification signée par son auteur, au relais de la
    /// région (voir `crate::sovereign`).
    Signed(crate::sovereign::Edit),
    /// (v224) Lumière de l'éditeur (projecteur, lumière ponctuelle) : demande d'un OP à
    /// l'arbitre du chunk `chunk` (le relais du monde, ou le relais de sa province, comme pour
    /// les objets) : y mettre la lumière `uid` telle que la décrit `light` (son texte, comme
    /// dans `lumieres.json`), ou l'en retirer (`None`). Une lumière qui change de zone est
    /// retirée de l'ancienne et mise dans la nouvelle (deux demandes, voir `lightnet`).
    LightPut {
        seq: u32,
        uid: u64,
        chunk: [i32; 2],
        light: Option<Vec<u8>>,
    },
    /// (v224) Lumière publiée par l'arbitre : gardée dans l'historique de la zone de `chunk`
    /// (la dernière de chaque lumière dans chaque zone), remise aux joueurs proches et au
    /// propriétaire (qui l'enregistre avec le monde). `None` : plus dans cette zone. `by`,
    /// `seq` : l'OP dont la demande vient d'être acceptée.
    Light {
        uid: u64,
        chunk: [i32; 2],
        light: Option<Vec<u8>>,
        by: String,
        seq: u32,
    },
    /// (v224) Demande de lumière `seq` refusée (pas OP, trop de demandes, monde en pause) :
    /// remise au seul demandeur, qui remet la lumière comme les autres l'ont.
    LightDenied { seq: u32 },
    /// (v227) Foule d'essai d'un OP (paquet de `crate::crowd`), envoyée comme ses positions
    /// (`Request::Nearby`) au relais qui les suit (relais du monde, ou de sa province) : il la
    /// remet aux joueurs à `CROWD_CHUNKS` chunks de lui au plus, avec son pseudo (`owner`,
    /// celui de sa session).
    Crowd { owner: String, data: Vec<u8> },
    /// (v227) Coup d'un joueur au PNJ `id` de la foule de `owner`, depuis ses yeux en `from` :
    /// remis par le relais au seul `owner`, qui mène la foule, avec le pseudo de la session
    /// de celui qui frappe (`by`).
    CrowdHit {
        owner: String,
        by: String,
        id: u16,
        from: [f64; 3],
    },
    /// (v227) Foule refusée (pas OP de ce serveur, trop de paquets, paquet trop gros ou
    /// absurde) : remis au seul demandeur, dont la foule s'arrête.
    CrowdDenied,
    /// (v229) Foudre de `/thor` ou de la Foudre de Zeus : demande d'un OP à l'arbitre du
    /// chunk `chunk` (celui de `pos` ; le relais du monde, ou le relais de sa province). Elle
    /// tombe en `pos`, ou (`up`) monte de `pos` vers le ciel dans la direction `dir`.
    /// `seed` : de quoi tirer sa forme, la même chez tous ceux qui la voient.
    ThorAsk {
        seq: u32,
        chunk: [i32; 2],
        up: bool,
        pos: [f64; 3],
        dir: [f32; 3],
        seed: u32,
    },
    /// (v229) Foudre publiée par l'arbitre aux joueurs proches (et à celui qui l'a
    /// demandée) : elle part chez chacun à la réception. Rien n'en est gardé (un joueur qui
    /// arrive ensuite ne la revoit pas). `by`, `seq` : l'OP dont la demande est acceptée.
    Thor {
        chunk: [i32; 2],
        up: bool,
        pos: [f64; 3],
        dir: [f32; 3],
        seed: u32,
        by: String,
        seq: u32,
    },
    /// (v229) Demande de foudre `seq` refusée, au seul demandeur. `why` : `THOR_NOT_OP`
    /// (réservée aux OP du serveur), ou `THOR_BUSY` (trop de demandes, arbitre pas prêt,
    /// monde en pause, point visé trop loin de soi). (v244) Aussi pour `ZeusAsk`.
    ThorDenied { seq: u32, why: u8 },
    /// (v244) Coup de la Foudre de Zeus légendaire, à l'arbitre du chunk `chunk` (celui de
    /// `pos`), avec le laissez-passer du relais du monde (`proto::zeus_permit_msg`, jusqu'à
    /// `expires`). `act` : `proto::ZEUS_STRIKE` (la foudre tombe du ciel en `pos`),
    /// `ZEUS_SKY` (elle monte de `pos` dans la direction `to`), `ZEUS_ARC` (le petit
    /// éclair, de `pos`, la pointe de l'éclair, à `to`).
    ZeusAsk {
        seq: u32,
        chunk: [i32; 2],
        act: u8,
        pos: [f64; 3],
        to: [f64; 3],
        seed: u32,
        expires: u64,
        permit: Vec<u8>,
    },
    /// (v244) Coup de la Foudre de Zeus publié par l'arbitre aux joueurs proches (et à celui
    /// qui l'a donné, `by`, demande `seq`) : il part chez chacun à la réception, comme
    /// `Thor`. Le petit éclair blesse chez lui le joueur qu'il touche.
    Zeus {
        chunk: [i32; 2],
        act: u8,
        pos: [f64; 3],
        to: [f64; 3],
        seed: u32,
        by: String,
        seq: u32,
    },
    /// (v312) Les Évidés que mène un joueur (paquet de `crate::evide`), envoyés comme ses
    /// positions (`Request::Nearby`) au relais qui les suit : il les surveille
    /// (`evide::Watch` : place, vie, morceaux mangés) et remet aux joueurs à `evide::CHUNKS`
    /// chunks de lui ce qu'il en admet, avec le pseudo de sa session (`owner`).
    Evide { owner: String, data: Vec<u8> },
    /// (v312) L'Évidé `id` de `owner` mord `victim` (`low` : en rampant, aux jambes ; `aim` :
    /// le tirage de l'endroit visé, `seed` : celui de la morsure). Demandé par son meneur ; le
    /// relais ne le publie (aux joueurs proches, à la victime, au meneur) que si le monstre
    /// peut mordre maintenant un joueur qu'il voit à sa portée. La victime s'ôte chez elle
    /// `evide::BITE_HURTS` points de vie.
    EvideBite {
        owner: String,
        id: u16,
        victim: String,
        low: bool,
        aim: u32,
        seed: u32,
    },
    /// (v312) Un joueur mange le morceau (`part`, `cell`) de l'Évidé `id` de `owner`.
    /// Demandé par celui qui mange ; le relais ne le publie (aux joueurs proches, au meneur, à
    /// lui) que s'il est à portée et le morceau encore là, avec le pseudo de sa session (`by`)
    /// et la vie qui reste au monstre (`health` ; 0 : il meurt).
    EvideEat {
        owner: String,
        id: u16,
        part: u8,
        cell: [u8; 3],
        by: String,
        health: u8,
    },
    /// (v323) Les sortes des couches (de bas en haut, `registry::CARPET_KINDS`) de la pile de
    /// tapis posée en `pos` (place absolue) : demande de celui qui l'a posée, juste après la
    /// modification de son bloc, à l'arbitre de son chunk (le relais du monde, ou le relais de
    /// sa province, comme pour les objets). Le bloc ne dit que la hauteur et la sorte du
    /// dessus. (En fin d'énumération : l'historique enregistré des zones se relit.)
    CarpetPut { pos: [i32; 3], layers: Vec<u8> },
    /// (v323) Publiée par l'arbitre : gardée dans l'historique de la zone (la dernière de chaque
    /// case), remise aux joueurs proches et à qui y arrive. Le jeu ne la prend que si elle va
    /// avec le bloc de la case (`World::carpet_layers`).
    Carpet { pos: [i32; 3], layers: Vec<u8> },
}

/// (v323) Couches d'une pile de tapis acceptées dans `BuildMsg::CarpetPut` : de 2 à 16 (une
/// pile est un bloc de 16 tapis au plus), chacune une sorte de tapis (moins de 64).
pub fn carpet_sane(layers: &[u8]) -> bool {
    (2..=16).contains(&layers.len()) && layers.iter().all(|&k| k < 64)
}

pub const THOR_NOT_OP: u8 = 0;
pub const THOR_BUSY: u8 = 1;
/// (v244) `ZeusAsk` sans laissez-passer valable du relais du monde.
pub const THOR_NO_PERMIT: u8 = 2;

/// (v244) Portée du petit éclair de la Foudre de Zeus sans charge (blocs), un peu de jeu
/// compris (l'arbitre refuse au-delà).
pub const ZEUS_ARC_MAX: f32 = 9.0;

/// (v244) Le coup de la Foudre de Zeus `m` (`BuildMsg::ZeusAsk`) n'a-t-il rien d'absurde ?
/// Des nombres finis, un point dans les bornes du monde et dans le chunk annoncé ; vers le
/// ciel, une direction de longueur 1 vers le haut ; le petit éclair, pas plus long que
/// `ZEUS_ARC_MAX`.
pub fn zeus_sane(m: &BuildMsg) -> bool {
    use crate::proto::{ZEUS_ARC, ZEUS_SKY, ZEUS_STRIKE};
    let BuildMsg::ZeusAsk {
        chunk,
        act,
        pos,
        to,
        permit,
        ..
    } = m
    else {
        return false;
    };
    let finite = pos.iter().chain(to.iter()).all(|v| v.is_finite());
    // (v318) Sur la carte bouclée, en double précision.
    if !finite
        || permit.len() > 128
        || !crate::proto::sane_place(*pos)
        || !(-64.0..=4096.0).contains(&pos[1])
    {
        return false;
    }
    let at = crate::proto::chunk_at(pos[0], pos[2]);
    // (Le petit éclair : l'écart le plus court, même de part et d'autre de la limite.)
    let d = [
        crate::proto::place_gap(to[0], pos[0]),
        to[1] - pos[1],
        crate::proto::place_gap(to[2], pos[2]),
    ];
    let len2 = to.iter().map(|v| v * v).sum::<f64>();
    let arc = ZEUS_ARC_MAX as f64;
    at == *chunk
        && match *act {
            ZEUS_STRIKE => true,
            ZEUS_SKY => (0.9..=1.1).contains(&len2) && to[1] > 0.05,
            ZEUS_ARC => d.iter().map(|v| v * v).sum::<f64>() <= arc * arc,
            _ => false,
        }
}

/// (v229) La demande de foudre `m` (`BuildMsg::ThorAsk`) n'a-t-elle rien d'absurde ? Des
/// nombres finis, un point dans les bornes du monde et dans le chunk annoncé, une
/// direction de longueur 1 (vers le haut pour une foudre qui monte).
pub fn thor_sane(m: &BuildMsg) -> bool {
    let BuildMsg::ThorAsk {
        chunk,
        up,
        pos,
        dir,
        ..
    } = m
    else {
        return false;
    };
    let finite = pos.iter().all(|v| v.is_finite()) && dir.iter().all(|v| v.is_finite());
    // (v318) Sur la carte bouclée, en double précision.
    if !finite || !crate::proto::sane_place(*pos) || !(-64.0..=4096.0).contains(&pos[1]) {
        return false;
    }
    let at = crate::proto::chunk_at(pos[0], pos[2]);
    let len2 = dir.iter().map(|v| v * v).sum::<f32>();
    at == *chunk && (!*up || ((0.9..=1.1).contains(&len2) && dir[1] > 0.05))
}

/// (v227) Portée de la foule d'essai d'un OP (chunks autour de lui) : ses PNJ vont jusqu'à
/// 200 blocs de lui, ceux qui les voient sont un peu plus loin.
pub const CROWD_CHUNKS: i32 = 24;
/// (v227) Taille d'un paquet de la foule, au plus (octets).
pub const CROWD_MAX_BYTES: usize = 24 * 1024;

/// (v224) Taille maximale du texte d'une lumière (`BuildMsg::LightPut`).
pub const LIGHT_MAX_BYTES: usize = 2048;

/// La position que décrit un `Player` (pour la remettre regroupée avec d'autres), si elle
/// n'a rien d'absurde.
pub fn pose_of(data: &[u8]) -> Option<(String, crate::poses::Pose)> {
    match decode::<BuildMsg>(data)? {
        BuildMsg::Player {
            pseudo,
            pos,
            yaw,
            pitch,
            crouch,
            speed,
            ping,
            held,
        } => {
            let pose = crate::poses::Pose {
                pos,
                yaw,
                pitch,
                crouch,
                speed,
                ping: (ping > 0).then_some(ping),
                held: crate::poses::Held::of(held).map_or(0, crate::poses::Held::byte),
            };
            pose.sane().then_some((pseudo, pose))
        }
        _ => None,
    }
}

/// Le bloc changé que décrit un `Set` (pour l'envoyer regroupé avec d'autres).
pub fn cell_of(data: &[u8]) -> Option<crate::cells::Cell> {
    match decode::<BuildMsg>(data)? {
        BuildMsg::Set {
            pos, block, seq, ..
        } => Some((pos, block, seq)),
        _ => None,
    }
}

/// Sortes de `ToPeer::Step`.
pub const STEP_RUN: u8 = 0;
pub const STEP_LAND: u8 = 1;
pub const STEP_PAIN: u8 = 2;
/// Plongeon dans l'eau d'assez haut.
pub const STEP_SPLASH: u8 = 3;
/// On grimpe ou descend une échelle à pleine vitesse (pas accroupi).
pub const STEP_LADDER: u8 = 4;

/// Fil des éliminations : à travers un mur, à travers une fumée, tireur aveuglé.
pub const KILL_WALL: u8 = 1;
pub const KILL_SMOKE: u8 = 2;
pub const KILL_BLIND: u8 = 4;

/// Arme posée au sol.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct GroundGun {
    pub id: u32,
    pub weapon: u8,
    pub pos: [f32; 3],
    /// D'où elle a été lancée (pour animer sa chute ; `pos` si elle est juste tombée).
    pub from: [f32; 3],
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum ToPeer {
    Welcome {
        you: u64,
        tick: u32,
        /// L'hôte n'a pas la même carte : la partie ne peut pas se jouer.
        map_ok: bool,
    },
    /// État de la partie au pas `tick` : le sien (et le dernier pas d'entrées pris en
    /// compte) et celui des autres.
    Snapshot {
        tick: u32,
        ack: u32,
        you: SimState,
        hp: u8,
        alive: bool,
        players: Vec<NetPlayer>,
        hud: MatchHud,
    },
    /// Entre deux `Snapshot` (envoyés 64 fois par seconde) : seulement ce qui bouge, à
    /// chaque pas (256 fois par seconde).
    Moves {
        tick: u32,
        ack: u32,
        you: SimState,
        players: Vec<Pose>,
    },
    /// Achat accepté : l'objet à donner.
    Give {
        item: u16,
    },
    /// Ce que le joueur porte vraiment selon l'hôte (armes, grenades par type) : son jeu
    /// s'y remet s'il s'en est écarté.
    Guns {
        primary: u8,
        secondary: u8,
        nades: [u8; 5],
    },
    /// Tableau des scores (toutes les demi-secondes).
    Scoreboard {
        rows: Vec<ScoreRow>,
        /// Vainqueur de chaque manche jouée (équipe de départ 0/1, raison).
        history: Vec<(u8, u8)>,
    },
    RoundEnd {
        winner: u8,
        reason: u8,
    },
    /// Fin de partie : résultat (aussi envoyé au relais pour le classement).
    GameOver {
        summary: MatchSummary,
    },
    Throw {
        owner: u64,
        nade: u8,
        pos: [f32; 3],
        vel: [f32; 3],
    },
    /// État complet de la partie pour le garant de secours (toutes les secondes), en
    /// morceaux (un message du relais fait au plus 64 Kio) : morceau `part` sur `of`.
    Backup {
        part: u16,
        of: u16,
        data: Vec<u8>,
    },
    /// Exclu de la partie par les règles (trois coéquipiers tués).
    Kicked {
        reason: String,
    },
    /// Armes au sol (liste complète, à chaque changement).
    Ground {
        guns: Vec<GroundGun>,
    },
    /// Le tir a touché (marqueur de touche).
    Hit {
        damage: u8,
        head: bool,
    },
    /// Fil des éliminations.
    Kill {
        killer: String,
        victim: String,
        weapon: u8,
        head: bool,
        /// `KILL_WALL`, `KILL_SMOKE`, `KILL_BLIND`.
        flags: u8,
        /// Équipes du tueur (255 : personne) et du mort.
        teams: [u8; 2],
    },
    /// Un joueur tire depuis `pos` (les autres l'entendent) ; `hits` : points où il
    /// touche un joueur (sang). (v249) `dirs` : la direction de chaque balle (chacun en tire
    /// les impacts sur les blocs, gardés jusqu'à la fin de la manche).
    Shot {
        who: u64,
        weapon: u8,
        pos: [f32; 3],
        hits: Vec<[f32; 3]>,
        dirs: Vec<[f32; 3]>,
    },
    /// Bruit d'un joueur proche en `pos` : `STEP_RUN` (il court), `STEP_LAND` (il retombe
    /// d'au moins 1,3 bloc), `STEP_PAIN` (une chute le blesse : cri de douleur).
    Step {
        who: u64,
        pos: [f32; 3],
        kind: u8,
    },
    /// Réapparition (après une mort) à cette position.
    Respawn {
        state: SimState,
        /// Nouvelle manche après une mort (ou changement de côté) : on repart avec le
        /// pistolet de départ ; sinon on garde ses armes.
        reset: bool,
        team: u8,
    },
    Chat {
        from: String,
        text: String,
    },
    /// Portes et trappes changées (case, bloc). `at` : où l'on entend la porte (aucun bruit
    /// pour l'état complet envoyé à l'arrivée d'un joueur).
    Doors {
        cells: Vec<([i32; 3], u16)>,
        at: Option<[f32; 3]>,
    },
    /// Voie rapide (`crate::fast`) : sonde envoyée par les passeurs à un joueur qui reçoit
    /// plus vite par le relais, au pas `tick`. S'il l'a avant l'état de ce pas, les
    /// passeurs sont redevenus les plus rapides.
    FastPing {
        tick: u32,
    },
    /// (v337) Par le relais (qui remet tout, dans l'ordre) : des positions (`Moves`) ou un
    /// état complet (`Snapshot`) écrits par différence avec celui du pas `base` envoyé
    /// avant par le même chemin (`delta`, `undelta`). Sans lui, le joueur le dit
    /// (`ToHost::DeltaMiss`) et reçoit de nouveau des messages entiers.
    MovesDelta {
        base: u32,
        data: Vec<u8>,
    },
    SnapDelta {
        base: u32,
        data: Vec<u8>,
    },
    /// (v338) Par la voie rapide (des paquets peuvent s'y perdre) : de même, mais la base
    /// est le dernier message du même genre que le joueur a confirmé
    /// (`ToHost::InputsFastPacked`). Sans elle, rien à redemander : sa confirmation
    /// suivante en donne une autre.
    FastMovesDelta {
        base: u32,
        data: Vec<u8>,
    },
    FastSnapDelta {
        base: u32,
        data: Vec<u8>,
    },
}

/// (v338) La différence `d` (de `delta`), pour la voie rapide.
pub fn fast_delta(d: ToPeer) -> ToPeer {
    match d {
        ToPeer::MovesDelta { base, data } => ToPeer::FastMovesDelta { base, data },
        ToPeer::SnapDelta { base, data } => ToPeer::FastSnapDelta { base, data },
        other => other,
    }
}

pub fn encode<T: Serialize>(m: &T) -> Vec<u8> {
    postcard::to_allocvec(m).unwrap_or_default()
}

pub fn decode<'a, T: Deserialize<'a>>(b: &'a [u8]) -> Option<T> {
    postcard::from_bytes(b).ok()
}

// ───────────── (v337) Différences sur le chemin du relais ─────────────
//
// Sur le pire chemin (ni passeur ni TURN : tout par les serveurs relais d'iroh), ce que
// l'hôte envoie à ses joueurs passe par une seule connexion, limitée (environ 120 Ko/s
// mesurés). Positions et états complets y partent donc écrits par différence avec le
// message du même genre envoyé juste avant par le même chemin : le relais remet tout, dans
// l'ordre. Chaque champ est un mot de 32 bits (un flottant par ses bits : la différence se
// défait exactement) ; seuls les mots qui changent partent, en « ou exclusif » avec le mot
// d'avant (petit nombre, peu d'octets en varint), derrière un masque.

fn put_var(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn get_var(inp: &mut &[u8]) -> Option<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let (&b, rest) = inp.split_first()?;
        *inp = rest;
        v |= u64::from(b & 0x7F) << shift;
        if b < 0x80 {
            return Some(v);
        }
    }
    None
}

fn zz(v: i64) -> u32 {
    ((v << 1) ^ (v >> 63)) as u32
}

fn unzz(v: u32) -> i64 {
    i64::from(v >> 1) ^ -i64::from(v & 1)
}

/// Les mots qui changent : leur masque, puis chacun par son écart avec l'ancien (sur 32
/// bits, en zigzag : un petit écart, dans un sens ou dans l'autre, tient sur un octet ; pour
/// un flottant, l'écart de ses bits est le nombre de pas de flottant entre les deux).
fn put_words(out: &mut Vec<u8>, base: &[u32], new: &[u32]) {
    let mut mask = 0u64;
    for (i, (b, n)) in base.iter().zip(new).enumerate() {
        if b != n {
            mask |= 1 << i;
        }
    }
    put_var(out, mask);
    for (i, (b, n)) in base.iter().zip(new).enumerate() {
        if mask >> i & 1 == 1 {
            put_var(out, u64::from(zz(i64::from(n.wrapping_sub(*b) as i32))));
        }
    }
}

fn get_words(inp: &mut &[u8], base: &[u32]) -> Option<Vec<u32>> {
    let mask = get_var(inp)?;
    if base.len() < 64 && mask >> base.len() != 0 {
        return None;
    }
    let mut w = base.to_vec();
    for (i, x) in w.iter_mut().enumerate() {
        if mask >> i & 1 == 1 {
            let d = unzz(u32::try_from(get_var(inp)?).ok()?) as i32;
            *x = x.wrapping_add(d as u32);
        }
    }
    Some(w)
}

/// Octets d'un petit bloc (le HUD) : même longueur, ceux qui changent derrière un masque par
/// paquet de 8 ; sinon le bloc entier.
fn put_bytes(out: &mut Vec<u8>, base: &[u8], new: &[u8]) {
    if base.len() != new.len() {
        out.push(0);
        put_var(out, new.len() as u64);
        out.extend_from_slice(new);
        return;
    }
    out.push(1);
    for (b, n) in base.chunks(8).zip(new.chunks(8)) {
        let mut mask = 0u8;
        for (i, (x, y)) in b.iter().zip(n).enumerate() {
            if x != y {
                mask |= 1 << i;
            }
        }
        out.push(mask);
        for (i, y) in n.iter().enumerate() {
            if mask >> i & 1 == 1 {
                out.push(*y);
            }
        }
    }
}

fn get_bytes(inp: &mut &[u8], base: &[u8]) -> Option<Vec<u8>> {
    let (&kind, rest) = inp.split_first()?;
    *inp = rest;
    if kind == 0 {
        let n = usize::try_from(get_var(inp)?).ok()?;
        if inp.len() < n {
            return None;
        }
        let (v, rest) = inp.split_at(n);
        *inp = rest;
        return Some(v.to_vec());
    }
    let mut v = base.to_vec();
    for chunk in v.chunks_mut(8) {
        let (&mask, rest) = inp.split_first()?;
        *inp = rest;
        for (i, x) in chunk.iter_mut().enumerate() {
            if mask >> i & 1 == 1 {
                let (&y, rest) = inp.split_first()?;
                *inp = rest;
                *x = y;
            }
        }
    }
    Some(v)
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    put_var(out, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
}

fn get_str(inp: &mut &[u8]) -> Option<String> {
    let n = usize::try_from(get_var(inp)?).ok()?;
    if inp.len() < n {
        return None;
    }
    let (v, rest) = inp.split_at(n);
    *inp = rest;
    String::from_utf8(v.to_vec()).ok()
}

/// L'état de soi : ce qui change le plus d'abord (masque sur un octet le plus souvent).
fn sim_words(s: &SimState) -> [u32; 13] {
    [
        s.pos[0].to_bits(),
        s.pos[1].to_bits(),
        s.pos[2].to_bits(),
        s.vel[0].to_bits(),
        s.vel[1].to_bits(),
        s.vel[2].to_bits(),
        s.stamina.to_bits(),
        s.crouch.to_bits(),
        s.jump_buffer.to_bits(),
        s.duck_speed.to_bits(),
        u32::from(s.on_ground),
        u32::from(s.sneak_held),
        u32::from(s.carry),
    ]
}

fn sim_from(w: &[u32]) -> Option<SimState> {
    let f = |i: usize| f32::from_bits(w[i]);
    Some(SimState {
        pos: [f(0), f(1), f(2)],
        vel: [f(3), f(4), f(5)],
        stamina: f(6),
        crouch: f(7),
        jump_buffer: f(8),
        duck_speed: f(9),
        on_ground: w[10] != 0,
        sneak_held: w[11] != 0,
        carry: u8::try_from(w[12]).ok()?,
    })
}

/// (v337) La position de celui qui reçoit, arrondie en `POSE_UNIT`es : ajoutée aux
/// positions relatives, elle donne des positions qui ne bougent que quand le joueur bouge
/// (pas quand celui qui reçoit bouge). Calculée de la même façon des deux côtés, à partir
/// des bits exacts de `you.pos`.
fn pose_anchor(you: &SimState) -> [u32; 3] {
    let q = |v: f32| (f64::from(v) * f64::from(POSE_UNIT)).round() as i64 as i32 as u32;
    [q(you.pos[0]), q(you.pos[1]), q(you.pos[2])]
}

fn pose_words(p: &Pose, a: [u32; 3]) -> [u32; 7] {
    [
        (p.rel[0] as u32).wrapping_add(a[0]),
        (p.rel[1] as u32).wrapping_add(a[1]),
        (p.rel[2] as u32).wrapping_add(a[2]),
        zz(i64::from(p.yaw)),
        zz(i64::from(p.pitch)),
        u32::from(p.crouch),
        u32::from(p.speed),
    ]
}

fn pose_from(id: u64, w: &[u32], a: [u32; 3]) -> Option<Pose> {
    Some(Pose {
        id,
        rel: [
            w[0].wrapping_sub(a[0]) as i32,
            w[1].wrapping_sub(a[1]) as i32,
            w[2].wrapping_sub(a[2]) as i32,
        ],
        yaw: i16::try_from(unzz(w[3])).ok()?,
        pitch: i16::try_from(unzz(w[4])).ok()?,
        crouch: u8::try_from(w[5]).ok()?,
        speed: u8::try_from(w[6]).ok()?,
    })
}

/// Un joueur d'un état complet, tel qu'il est sur le fil (angles, accroupi et vitesse
/// arrondis comme le font `turn16fixed`, `turn16`, `unit8`, `tenth8`).
fn player_words(p: &NetPlayer) -> [u32; 14] {
    let turn = |a: f32| (a / std::f32::consts::TAU * 65536.0).round() as i64 as i16;
    [
        p.pos[0].to_bits(),
        p.pos[1].to_bits(),
        p.pos[2].to_bits(),
        u32::from(turn(p.yaw) as u16),
        zz(i64::from(turn(p.pitch))),
        u32::from((p.speed * 10.0).clamp(0.0, 255.0).round() as u8),
        u32::from((p.crouch.clamp(0.0, 1.0) * 255.0).round() as u8),
        u32::from(p.hp),
        u32::from(p.alive),
        u32::from(p.weapon),
        u32::from(p.kills),
        u32::from(p.deaths),
        u32::from(p.team),
        u32::from(p.flags),
    ]
}

fn player_from(id: u64, pseudo: String, w: &[u32]) -> Option<NetPlayer> {
    let angle = |v: i16| v as f32 / 65536.0 * std::f32::consts::TAU;
    let f = |i: usize| f32::from_bits(w[i]);
    Some(NetPlayer {
        id,
        pseudo,
        pos: [f(0), f(1), f(2)],
        yaw: angle(u16::try_from(w[3]).ok()? as i16),
        pitch: angle(i16::try_from(unzz(w[4])).ok()?),
        speed: f32::from(u8::try_from(w[5]).ok()?) / 10.0,
        crouch: f32::from(u8::try_from(w[6]).ok()?) / 255.0,
        hp: u8::try_from(w[7]).ok()?,
        alive: w[8] != 0,
        weapon: u8::try_from(w[9]).ok()?,
        kills: u16::try_from(w[10]).ok()?,
        deaths: u16::try_from(w[11]).ok()?,
        team: u8::try_from(w[12]).ok()?,
        flags: u8::try_from(w[13]).ok()?,
    })
}

/// Une liste de joueurs : chacun comparé à celui de même identifiant dans la liste
/// d'avant (sa place, plus un), ou entier (0, puis son identifiant). (v337) Mêmes joueurs
/// dans le même ordre (le plus souvent) : rien que les mots, sans les places. `base_words`
/// et `words` : les mots d'un joueur de la liste d'avant et de la nouvelle.
fn put_list<T, const N: usize>(
    out: &mut Vec<u8>,
    base: &[T],
    new: &[T],
    id: impl Fn(&T) -> u64,
    base_words: impl Fn(&T) -> [u32; N],
    words: impl Fn(&T) -> [u32; N],
    extra: impl Fn(&mut Vec<u8>, Option<&T>, &T),
) {
    let same = base.len() == new.len() && base.iter().zip(new).all(|(b, n)| id(b) == id(n));
    put_var(out, (new.len() as u64) << 1 | u64::from(same));
    for (i, e) in new.iter().enumerate() {
        let at = if same {
            Some(i)
        } else {
            base.iter().position(|b| id(b) == id(e))
        };
        match at {
            Some(j) => {
                if !same {
                    put_var(out, j as u64 + 1);
                }
                put_words(out, &base_words(&base[j]), &words(e));
                extra(out, Some(&base[j]), e);
            }
            None => {
                put_var(out, 0);
                put_var(out, id(e));
                put_words(out, &[0; N], &words(e));
                extra(out, None, e);
            }
        }
    }
}

/// La place, dans la liste d'avant, du joueur suivant de la liste lue (`put_list`) : `None`
/// pour un nouveau.
fn get_at(inp: &mut &[u8], same: bool, i: usize) -> Option<Option<usize>> {
    if same {
        return Some(Some(i));
    }
    let at = usize::try_from(get_var(inp)?).ok()?;
    Some(at.checked_sub(1))
}

fn input_words(i: &NetInput) -> [u32; 6] {
    [
        i.yaw.to_bits(),
        i.pitch.to_bits(),
        u32::from(i.forward as u8),
        u32::from(i.strafe as u8),
        u32::from(i.jump)
            | u32::from(i.jump_pressed) << 1
            | u32::from(i.walk) << 2
            | u32::from(i.duck) << 3,
        u32::from(i.carry),
    ]
}

/// (v337) Des pas d'entrées à la suite : le premier entier, chacun des suivants par
/// différence avec celui d'avant (le regard, par les bits de ses flottants : exact).
pub fn pack_inputs(inputs: &[NetInput]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + inputs.len() * 4);
    put_var(&mut out, inputs.len() as u64);
    let mut prev = [0u32; 6];
    for i in inputs {
        let w = input_words(i);
        put_words(&mut out, &prev, &w);
        prev = w;
    }
    out
}

pub fn unpack_inputs(data: &[u8]) -> Option<Vec<NetInput>> {
    let mut inp = data;
    let n = usize::try_from(get_var(&mut inp)?).ok()?;
    if n > 4096 {
        return None;
    }
    let mut v = Vec::with_capacity(n);
    let mut prev = [0u32; 6];
    for _ in 0..n {
        let w = get_words(&mut inp, &prev)?;
        if w[4] > 15 {
            return None;
        }
        v.push(NetInput {
            yaw: f32::from_bits(w[0]),
            pitch: f32::from_bits(w[1]),
            forward: u8::try_from(w[2]).ok()? as i8,
            strafe: u8::try_from(w[3]).ok()? as i8,
            jump: w[4] & 1 != 0,
            jump_pressed: w[4] & 2 != 0,
            walk: w[4] & 4 != 0,
            duck: w[4] & 8 != 0,
            carry: u8::try_from(w[5]).ok()?,
        });
        prev.copy_from_slice(&w);
    }
    inp.is_empty().then_some(v)
}

/// `base` sur le pas `tick` et `new` ensuite : la différence, ou rien si ce ne sont pas deux
/// messages du même genre (positions, ou états complets).
pub fn delta(base_tick: u32, base: &ToPeer, new: &ToPeer) -> Option<ToPeer> {
    let mut out = Vec::new();
    match (base, new) {
        (
            ToPeer::Moves {
                tick: bt,
                ack: ba,
                you: by,
                players: bp,
            },
            ToPeer::Moves {
                tick,
                ack,
                you,
                players,
            },
        ) => {
            put_var(&mut out, u64::from(tick.wrapping_sub(*bt)));
            put_var(&mut out, u64::from(zz(i64::from(*ack) - i64::from(*ba))));
            put_words(&mut out, &sim_words(by), &sim_words(you));
            let (ab, an) = (pose_anchor(by), pose_anchor(you));
            put_list(
                &mut out,
                bp,
                players,
                |p| p.id,
                |p| pose_words(p, ab),
                |p| pose_words(p, an),
                |_, _, _| {},
            );
            Some(ToPeer::MovesDelta {
                base: base_tick,
                data: out,
            })
        }
        (
            ToPeer::Snapshot {
                tick: bt,
                ack: ba,
                you: by,
                hp: bh,
                alive: bl,
                players: bp,
                hud: bhud,
            },
            ToPeer::Snapshot {
                tick,
                ack,
                you,
                hp,
                alive,
                players,
                hud,
            },
        ) => {
            put_var(&mut out, u64::from(tick.wrapping_sub(*bt)));
            put_var(&mut out, u64::from(zz(i64::from(*ack) - i64::from(*ba))));
            put_words(&mut out, &sim_words(by), &sim_words(you));
            put_words(
                &mut out,
                &[u32::from(*bh), u32::from(*bl)],
                &[u32::from(*hp), u32::from(*alive)],
            );
            put_list(
                &mut out,
                bp,
                players,
                |p| p.id,
                player_words,
                player_words,
                |out, b, p| match b {
                    Some(b) if b.pseudo == p.pseudo => out.push(0),
                    _ => {
                        out.push(1);
                        put_str(out, &p.pseudo);
                    }
                },
            );
            put_bytes(&mut out, &encode(bhud), &encode(hud));
            Some(ToPeer::SnapDelta {
                base: base_tick,
                data: out,
            })
        }
        _ => None,
    }
}

/// Le message entier, à partir de `base` (le message du pas `base` de la différence) ;
/// rien si `d` n'en est pas une, ne va pas avec `base`, ou est abîmée.
pub fn undelta(base: &ToPeer, d: &ToPeer) -> Option<ToPeer> {
    match (base, d) {
        (
            ToPeer::Moves {
                tick: bt,
                ack: ba,
                you: by,
                players: bp,
            },
            ToPeer::MovesDelta { data, .. } | ToPeer::FastMovesDelta { data, .. },
        ) => {
            let mut inp = &data[..];
            let tick = bt.wrapping_add(u32::try_from(get_var(&mut inp)?).ok()?);
            let ack = u32::try_from(i64::from(*ba) + unzz(u32::try_from(get_var(&mut inp)?).ok()?))
                .ok()?;
            let you = sim_from(&get_words(&mut inp, &sim_words(by))?)?;
            let (ab, an) = (pose_anchor(by), pose_anchor(&you));
            let head = get_var(&mut inp)?;
            let (n, same) = (usize::try_from(head >> 1).ok()?, head & 1 == 1);
            if same && n != bp.len() {
                return None;
            }
            let mut players = Vec::with_capacity(n.min(256));
            for i in 0..n {
                let p = match get_at(&mut inp, same, i)? {
                    None => {
                        let id = get_var(&mut inp)?;
                        pose_from(id, &get_words(&mut inp, &[0; 7])?, an)?
                    }
                    Some(j) => {
                        let b = bp.get(j)?;
                        pose_from(b.id, &get_words(&mut inp, &pose_words(b, ab))?, an)?
                    }
                };
                players.push(p);
            }
            inp.is_empty().then_some(ToPeer::Moves {
                tick,
                ack,
                you,
                players,
            })
        }
        (
            ToPeer::Snapshot {
                tick: bt,
                ack: ba,
                you: by,
                hp: bh,
                alive: bl,
                players: bp,
                hud: bhud,
            },
            ToPeer::SnapDelta { data, .. } | ToPeer::FastSnapDelta { data, .. },
        ) => {
            let mut inp = &data[..];
            let tick = bt.wrapping_add(u32::try_from(get_var(&mut inp)?).ok()?);
            let ack = u32::try_from(i64::from(*ba) + unzz(u32::try_from(get_var(&mut inp)?).ok()?))
                .ok()?;
            let you = sim_from(&get_words(&mut inp, &sim_words(by))?)?;
            let vitals = get_words(&mut inp, &[u32::from(*bh), u32::from(*bl)])?;
            let head = get_var(&mut inp)?;
            let (n, same) = (usize::try_from(head >> 1).ok()?, head & 1 == 1);
            if same && n != bp.len() {
                return None;
            }
            let mut players = Vec::with_capacity(n.min(256));
            for i in 0..n {
                let (id, base_words, base_pseudo) = match get_at(&mut inp, same, i)? {
                    None => (get_var(&mut inp)?, [0u32; 14], None),
                    Some(j) => {
                        let b = bp.get(j)?;
                        (b.id, player_words(b), Some(&b.pseudo))
                    }
                };
                let w = get_words(&mut inp, &base_words)?;
                let (&changed, rest) = inp.split_first()?;
                inp = rest;
                let pseudo = match (changed, base_pseudo) {
                    (0, Some(p)) => p.clone(),
                    (1, _) => get_str(&mut inp)?,
                    _ => return None,
                };
                players.push(player_from(id, pseudo, &w)?);
            }
            let hud = decode::<MatchHud>(&get_bytes(&mut inp, &encode(bhud))?)?;
            inp.is_empty().then_some(ToPeer::Snapshot {
                tick,
                ack,
                you,
                hp: u8::try_from(vitals[0]).ok()?,
                alive: vitals[1] != 0,
                players,
                hud,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (v334) Positions relatives : à moins d'un centimètre près, même loin du centre du
    /// monde et loin de celui qui reçoit ; et ce que pèsent les messages d'une partie à
    /// 10 (identifiants du relais : petits nombres).
    #[test]
    fn poses_are_small_and_exact_to_the_centimetre() {
        let player = |i: u64, at: [f32; 3]| NetPlayer {
            id: 40 + i,
            pseudo: String::new(),
            pos: at,
            yaw: -2.9 + i as f32 * 0.7,
            pitch: 0.12,
            crouch: 0.0,
            speed: 4.5,
            hp: 87,
            alive: true,
            weapon: 3,
            kills: 7,
            deaths: 5,
            team: 1,
            flags: 0,
        };
        for anchor in [[1234.56, 70.1, -987.6], [1_000_123.4, 64.0, -2_000_456.7]] {
            for d in [
                [3.3, 0.0, -2.1],
                [41.27, 1.5, 17.03],
                [700.0, -20.0, -1500.0],
            ] {
                let p = player(1, [anchor[0] + d[0], anchor[1] + d[1], anchor[2] + d[2]]);
                let wire = encode(&Pose::of(&p, anchor));
                let mut back = player(1, [0.0; 3]);
                decode::<Pose>(&wire).unwrap().apply(&mut back, anchor);
                for k in 0..3 {
                    // (Loin du centre, un f32 lui-même n'a que 6 cm de finesse.)
                    let ulp = f32::EPSILON * p.pos[k].abs().max(1.0) * 2.0;
                    assert!(
                        (back.pos[k] - p.pos[k]).abs() <= 1.0 / 128.0 + ulp,
                        "{k} : {} pour {}",
                        back.pos[k],
                        p.pos[k]
                    );
                }
                assert!((back.yaw - p.yaw).abs() < 1e-3);
            }
        }
        let you = SimState {
            pos: [1234.56, 70.1, -987.6],
            vel: [3.2, -0.1, 5.5],
            on_ground: true,
            stamina: 0.8,
            duck_speed: 8.0,
            carry: 250,
            ..Default::default()
        };
        let others: Vec<NetPlayer> = (0..9)
            .map(|i| player(i, [1200.0 + i as f32 * 3.3, 70.0, -990.0 + i as f32 * 2.1]))
            .collect();
        let moves = ToPeer::Moves {
            tick: 123_456,
            ack: 123_400,
            you,
            players: others.iter().map(|p| Pose::of(p, you.pos)).collect(),
        };
        let snap = ToPeer::Snapshot {
            tick: 123_456,
            ack: 123_400,
            you,
            hp: 87,
            alive: true,
            players: others.clone(),
            hud: MatchHud {
                money: 4200,
                time: 75.3,
                round: 7,
                ..Default::default()
            },
        };
        let (m, sn) = (encode(&moves).len(), encode(&snap).len());
        eprintln!("10 joueurs : positions {m} octets, état complet {sn} octets");
        assert!(m <= 170, "positions : {m} octets (249 avant la v334)");
        assert!(sn <= 345, "état complet : {sn} octets");
    }

    /// Essais : une partie simulée à 10 où tout le monde court et tourne sans arrêt (le pire
    /// cas) ; le joueur 0 est celui qui reçoit.
    struct Run10 {
        x: u64,
        pos: Vec<[f32; 3]>,
        dir: Vec<f32>,
        yaw: Vec<f32>,
        pitch: [f32; 10],
        hp: [u8; 10],
        kills: [u16; 10],
    }

    impl Run10 {
        const DT: f32 = 1.0 / 256.0;

        fn new() -> Run10 {
            Run10 {
                x: 0x9E37_79B9_7F4A_7C15,
                // Loin du centre du monde.
                pos: (0..10)
                    .map(|i| [1234.5 + i as f32 * 4.0, 70.0, -987.25 - i as f32 * 3.0])
                    .collect(),
                dir: (0..10).map(|i| i as f32).collect(),
                yaw: (0..10).map(|i| i as f32 * 0.6).collect(),
                pitch: [0.05; 10],
                hp: [100; 10],
                kills: [0; 10],
            }
        }

        fn rnd(&mut self) -> f32 {
            self.x ^= self.x << 13;
            self.x ^= self.x >> 7;
            self.x ^= self.x << 17;
            (self.x >> 11) as f32 / (1u64 << 53) as f32
        }

        /// Un pas : tout le monde court (6,5 blocs par seconde), change de direction, tourne
        /// la tête (jusqu'à un demi-tour par seconde, et des coups de souris), saute parfois.
        fn step(&mut self, tick: u32) {
            let dt = Self::DT;
            for i in 0..10 {
                self.dir[i] += (self.rnd() - 0.5) * 0.2;
                self.pos[i][0] += self.dir[i].cos() * 6.5 * dt;
                self.pos[i][2] += self.dir[i].sin() * 6.5 * dt;
                self.pos[i][1] = 70.0 + ((tick as f32 + i as f32 * 37.0) * dt * 3.0).sin().max(0.0);
                let flick = if self.rnd() < 0.002 { 1.5 } else { 0.0 };
                self.yaw[i] += (self.rnd() - 0.5) * 0.05 + flick;
                self.pitch[i] = (self.pitch[i] + (self.rnd() - 0.5) * 0.01).clamp(-1.5, 1.5);
                if self.rnd() < 0.001 {
                    self.hp[i] = self.hp[i].saturating_sub(27);
                }
                if self.rnd() < 0.0005 {
                    self.kills[i] += 1;
                }
            }
        }

        /// Ce que le joueur 0 reçoit à ce pas : un état complet ou des positions.
        fn msg(&self, tick: u32, snap: bool) -> ToPeer {
            let (pos, dir, hp) = (&self.pos, &self.dir, &self.hp);
            let you = SimState {
                pos: pos[0],
                vel: [dir[0].cos() * 6.5, 0.0, dir[0].sin() * 6.5],
                on_ground: tick % 300 > 20,
                crouch: 0.0,
                stamina: 1.0 - (tick % 512) as f32 / 1024.0,
                duck_speed: 8.0,
                jump_buffer: 0.0,
                sneak_held: false,
                carry: 250,
            };
            // Les joueurs 7 et 8 sortent de la vue un moment, puis reviennent.
            let seen: Vec<usize> = (1..10)
                .filter(|&i| !(i >= 7 && (tick / 256) % 3 == 1))
                .collect();
            let player = |i: usize| NetPlayer {
                id: 40 + i as u64,
                pseudo: if tick < 64 {
                    format!("Joueur{i}")
                } else {
                    String::new()
                },
                pos: pos[i],
                yaw: self.yaw[i],
                pitch: self.pitch[i],
                crouch: if (tick / 128 + i as u32).is_multiple_of(5) {
                    1.0
                } else {
                    0.0
                },
                speed: 6.5,
                hp: hp[i],
                alive: hp[i] > 0,
                weapon: if (tick / 512 + i as u32).is_multiple_of(2) {
                    3
                } else {
                    7
                },
                kills: self.kills[i],
                deaths: 0,
                team: (i % 2) as u8,
                flags: 0,
            };
            if snap {
                ToPeer::Snapshot {
                    tick: 100_000 + tick,
                    ack: 99_000 + tick / 2,
                    you,
                    hp: hp[0],
                    alive: true,
                    players: seen.iter().map(|&i| player(i)).collect(),
                    hud: MatchHud {
                        money: 4200 + tick / 64,
                        time: 115.0 - tick as f32 * Self::DT,
                        round: 7,
                        ..Default::default()
                    },
                }
            } else {
                ToPeer::Moves {
                    tick: 100_000 + tick,
                    ack: 99_000 + tick / 2,
                    you,
                    players: seen
                        .iter()
                        .map(|&i| Pose::of(&player(i), you.pos))
                        .collect(),
                }
            }
        }
    }

    /// (v337) Différences sur le chemin du relais : sur une partie simulée à 10 où tout le
    /// monde court et tourne sans arrêt (le pire cas), au palier de 64 mises à jour par
    /// seconde (un état complet tous les 16 pas, des positions tous les 4), chaque message
    /// refait à partir de la différence est exactement celui qui serait parti entier, même
    /// quand des joueurs disparaissent de la vue et reviennent, changent de pseudo, d'arme ou
    /// meurent. Et ce que ça pèse, par joueur et par seconde, en-têtes du relais compris.
    #[test]
    fn differences_rebuild_the_exact_message_and_are_small() {
        let mut run = Run10::new();
        let (mut full, mut sent) = (0usize, 0usize);
        let (mut msgs, mut moves_n, mut snaps_n) = (0usize, 0usize, 0usize);
        let mut bases: [Option<(u32, ToPeer)>; 2] = [None, None];
        let wire = |m: &ToPeer| decode::<ToPeer>(&encode(m)).unwrap();
        for tick in 0u32..256 * 8 {
            run.step(tick);
            if tick % 4 != 0 {
                continue;
            }
            let msg = run.msg(tick, tick % 16 == 0);
            let k = usize::from(matches!(msg, ToPeer::Snapshot { .. }));
            let whole = encode(&msg).len();
            let back = match &bases[k] {
                Some((bt, b)) => {
                    let d = delta(*bt, b, &msg).expect("même genre");
                    let wire_d = encode(&d);
                    let got = undelta(b, &decode::<ToPeer>(&wire_d).unwrap()).expect("refait");
                    assert_eq!(encode(&got), encode(&msg), "pas {tick}");
                    full += whole;
                    sent += wire_d.len();
                    msgs += 1;
                    if k == 0 {
                        moves_n += 1;
                    } else {
                        snaps_n += 1;
                    }
                    got
                }
                None => wire(&msg),
            };
            bases[k] = Some((100_000 + tick, back));
        }
        // Une différence abîmée, ou appliquée au mauvais message : rien.
        let (bt, b) = bases[0].clone().unwrap();
        let (_, s) = bases[1].clone().unwrap();
        if let Some(ToPeer::MovesDelta { base, mut data }) = delta(bt, &b, &b) {
            assert!(undelta(
                &s,
                &ToPeer::MovesDelta {
                    base,
                    data: data.clone()
                }
            )
            .is_none());
            data.push(7);
            assert!(undelta(&b, &ToPeer::MovesDelta { base, data }).is_none());
        }
        // Par seconde, au palier de 64 : 48 positions et 16 états complets, chacun avec
        // l'en-tête du relais (trame, chiffrement, demande : 25 octets).
        let per = |n: usize, total: usize| total as f32 / n as f32;
        let avg_sent = per(msgs, sent);
        let avg_full = per(msgs, full);
        let rate = 64.0 * (avg_sent + 25.0) / 1024.0;
        eprintln!(
            "DIFFÉRENCES : {moves_n} positions et {snaps_n} états, {avg_sent:.0} octets en \
             moyenne au lieu de {avg_full:.0} ; à 64 par seconde, {rate:.1} Ko/s par joueur \
             ({:.1} avant), {:.0} Ko/s pour 9",
            64.0 * (avg_full + 25.0) / 1024.0,
            rate * 9.0
        );
        assert!(avg_sent < avg_full * 0.55, "{avg_sent} pour {avg_full}");
    }

    /// (v338) Par la voie rapide, des paquets se perdent : chaque différence part du dernier
    /// message du même genre que le joueur a confirmé, plus vieux que celui d'avant (le temps
    /// d'un aller-retour et des entrées qui portent la confirmation). Même partie à 10, à 128
    /// mises à jour par seconde (TURN : un état complet sur 4), base vieille de 8 pas (31 ms)
    /// ou 16 (62 ms) : chaque message refait est exact, et ce que reçoit chaque joueur par
    /// TURN, en-têtes de la voie rapide et IP/UDP compris (71 octets par paquet).
    #[test]
    fn fast_differences_from_a_confirmed_base_are_exact_and_small() {
        let wire = |m: &ToPeer| decode::<ToPeer>(&encode(m)).unwrap();
        let mut rates = Vec::new();
        for age in [8u32, 16] {
            let mut run = Run10::new();
            let mut sent: [Vec<(u32, ToPeer)>; 2] = Default::default();
            let (mut full, mut diff, mut n) = ([0usize; 2], [0usize; 2], [0usize; 2]);
            for tick in 0u32..256 * 8 {
                run.step(tick);
                if tick % 2 != 0 {
                    continue;
                }
                let snap = tick % 8 == 0;
                let k = usize::from(snap);
                let msg = run.msg(tick, snap);
                let base = sent[k].iter().rev().find(|(t, _)| tick - t >= age);
                if let Some((bt, b)) = base {
                    let d = fast_delta(delta(100_000 + bt, b, &msg).expect("même genre"));
                    assert!(matches!(
                        d,
                        ToPeer::FastMovesDelta { .. } | ToPeer::FastSnapDelta { .. }
                    ));
                    let wire_d = encode(&d);
                    let got = undelta(b, &decode::<ToPeer>(&wire_d).unwrap()).expect("refait");
                    assert_eq!(encode(&got), encode(&msg), "pas {tick}");
                    full[k] += encode(&msg).len();
                    diff[k] += wire_d.len();
                    n[k] += 1;
                }
                sent[k].push((tick, wire(&msg)));
            }
            let avg = |t: [usize; 2]| [t[0] as f32 / n[0] as f32, t[1] as f32 / n[1] as f32];
            // Par seconde : 96 positions et 32 états complets.
            let rate = |a: [f32; 2]| (96.0 * (a[0] + 71.0) + 32.0 * (a[1] + 71.0)) / 1024.0;
            let (f, d) = (avg(full), avg(diff));
            eprintln!(
                "VOIE RAPIDE, base vieille de {age} pas : positions {:.0} octets au lieu de {:.0}, \
                 états {:.0} au lieu de {:.0} ; par TURN, {:.1} Ko/s par joueur ({:.1} entiers)",
                d[0],
                f[0],
                d[1],
                f[1],
                rate(d),
                rate(f)
            );
            assert!(rate(d) < rate(f) * 0.75, "{} pour {}", rate(d), rate(f));
            rates.push(rate(d));
        }
        assert!(rates[0] <= rates[1], "une base plus récente coûte moins");
    }

    /// (v338) Les entrées par TURN : 128 paquets par seconde, chacun redit les 4 derniers
    /// pas ; entiers (`InputsFast`) ou serrés avec la confirmation des derniers messages
    /// reçus (`InputsFastPacked`). Regard qui change à chaque pas (le pire cas) ou à chaque
    /// image (128 images par seconde : tous les deux pas).
    #[test]
    fn fast_inputs_through_turn_are_shorter_packed() {
        let mut yaw = 2.5f32;
        let all: Vec<NetInput> = (0..64)
            .map(|k| {
                yaw += 0.0093 * (k as f32 * 0.21).sin();
                NetInput {
                    forward: 1,
                    strafe: if (k / 24) % 2 == 0 { -1 } else { 1 },
                    jump: k == 40,
                    jump_pressed: k == 40,
                    walk: false,
                    duck: false,
                    yaw,
                    pitch: -0.08 + k as f32 * 0.0003,
                    carry: 230,
                }
            })
            .collect();
        let per_frame: Vec<NetInput> = all
            .iter()
            .enumerate()
            .map(|(k, i)| all[k - k % 2].clone_with(i))
            .collect();
        for (name, inputs) in [("à chaque pas", &all), ("à chaque image", &per_frame)] {
            let (mut whole, mut packed, mut n) = (0usize, 0usize, 0usize);
            for k in (4..inputs.len()).step_by(2) {
                let four = inputs[k - 4..k].to_vec();
                let seq = 1_000_000 + k as u32;
                whole += encode(&ToHost::InputsFast {
                    first_seq: seq,
                    inputs: four.clone(),
                    weapon: 3,
                })
                .len();
                packed += encode(&ToHost::InputsFastPacked {
                    first_seq: seq,
                    data: pack_inputs(&four),
                    weapon: 3,
                    moves: 1_000_000 + 2 * k as u32,
                    snap: 1_000_000 + 8 * (k as u32 / 4),
                })
                .len();
                n += 1;
            }
            let (w, p) = (whole as f32 / n as f32, packed as f32 / n as f32);
            eprintln!(
                "ENTRÉES PAR TURN, regard {name} : {p:.0} octets au lieu de {w:.0} ; \
                 {:.1} Ko/s au lieu de {:.1} (71 octets d'en-têtes par paquet)",
                128.0 * (p + 71.0) / 1024.0,
                128.0 * (w + 71.0) / 1024.0
            );
            assert!(p < w * 0.75, "{p} pour {w}");
        }
    }

    impl NetInput {
        /// Ce pas, avec le regard de `self` (essais).
        fn clone_with(&self, other: &NetInput) -> NetInput {
            NetInput {
                yaw: self.yaw,
                pitch: self.pitch,
                ..*other
            }
        }
    }

    /// (v337) Entrées serrées : exactement les mêmes une fois défaites, et bien plus courtes
    /// (16 pas d'un joueur qui court en tournant la tête).
    #[test]
    fn packed_inputs_are_exact_and_short() {
        let mut yaw = 1.234f32;
        let inputs: Vec<NetInput> = (0..16)
            .map(|k| {
                yaw += 0.0071 * (k as f32 * 0.3).sin();
                NetInput {
                    forward: 1,
                    strafe: if k < 8 { -1 } else { 1 },
                    jump: k == 9,
                    jump_pressed: k == 9,
                    walk: false,
                    duck: false,
                    yaw,
                    pitch: -0.12 + k as f32 * 0.0004,
                    carry: 230,
                }
            })
            .collect();
        let packed = pack_inputs(&inputs);
        assert_eq!(unpack_inputs(&packed), Some(inputs.clone()));
        let whole = encode(&inputs).len();
        // Le regard change à chaque pas (le pire cas) ; d'habitude il change à chaque image
        // (4 pas à 64 images par seconde) : les pas d'une même image ne coûtent qu'un octet.
        let held: Vec<NetInput> = inputs
            .iter()
            .enumerate()
            .map(|(k, i)| inputs[k - k % 4].clone_with(i))
            .collect();
        let packed_held = pack_inputs(&held);
        assert_eq!(unpack_inputs(&packed_held), Some(held));
        eprintln!(
            "ENTRÉES : 16 pas, {} octets serrés au lieu de {whole} ; regard par image : {}",
            packed.len(),
            packed_held.len()
        );
        assert!(
            packed.len() * 10 < whole * 6,
            "{} pour {whole}",
            packed.len()
        );
        assert!(
            packed_held.len() * 4 < whole,
            "{} pour {whole}",
            packed_held.len()
        );
        assert!(unpack_inputs(&packed[..packed.len() - 1]).is_none());
        assert_eq!(unpack_inputs(&pack_inputs(&[])), Some(Vec::new()));
    }

    #[test]
    fn spawns_change_the_map_signature_but_not_the_name() {
        let mut m = MapPackage {
            seed: 7,
            generator: "procedural".into(),
            ..Default::default()
        };
        let a = m.signature();
        m.fps.ct = Some(Zone::new([0, 10, 0], [4, 10, 4]));
        let b = m.signature();
        assert_ne!(a, b);
        m.fps.ct = Some(Zone::new([4, 10, 4], [0, 10, 0]));
        assert_eq!(b, m.signature(), "même zone, coins dans l'autre sens");
    }

    /// (v222) L'objet tenu (lampe torche, éclair) reste dans la position que le relais
    /// regroupe avec d'autres (`pose_of`) ; un octet absurde y devient « rien ».
    #[test]
    fn the_held_item_stays_in_the_regrouped_position() {
        let player = |held: u8| {
            encode(&BuildMsg::Player {
                pseudo: "Lampe".into(),
                pos: [3.0, 70.0, -8.0],
                yaw: 1.0,
                pitch: -0.2,
                crouch: 0.0,
                speed: 4.0,
                ping: 40,
                held,
            })
        };
        let on = crate::poses::Lamp { on: true, step: 7 }.byte();
        let (name, pose) = pose_of(&player(on)).expect("position");
        assert_eq!((name.as_str(), pose.held), ("Lampe", on));
        let zeus = crate::poses::Held::Zeus.byte();
        assert_eq!(pose_of(&player(zeus)).expect("position").1.held, zeus);
        assert_eq!(pose_of(&player(0)).expect("position").1.held, 0);
        assert_eq!(pose_of(&player(0x80 | 0x20)).expect("position").1.held, 0);
        assert_eq!(pose_of(&player(0x21)).expect("position").1.held, 0);
    }
}
