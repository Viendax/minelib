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
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub id: u64,
    pub pos: [f32; 3],
    pub yaw: i16,
    pub pitch: i16,
    pub crouch: u8,
    pub speed: u8,
}

impl Pose {
    pub fn of(p: &NetPlayer) -> Self {
        let turn = |a: f32| (a / std::f32::consts::TAU * 65536.0).round() as i64 as i16;
        Pose {
            id: p.id,
            pos: p.pos,
            yaw: turn(p.yaw),
            pitch: turn(p.pitch),
            crouch: (p.crouch.clamp(0.0, 1.0) * 255.0) as u8,
            speed: (p.speed * 10.0).clamp(0.0, 255.0) as u8,
        }
    }

    /// Met `p` à cette position.
    pub fn apply(&self, p: &mut NetPlayer) {
        let angle = |a: i16| a as f32 / 65536.0 * std::f32::consts::TAU;
        p.pos = self.pos;
        p.yaw = angle(self.yaw);
        p.pitch = angle(self.pitch);
        p.crouch = self.crouch as f32 / 255.0;
        p.speed = self.speed as f32 / 10.0;
    }
}

/// Un joueur vu par les autres.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct NetPlayer {
    pub id: u64,
    pub pseudo: String,
    pub pos: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub crouch: f32,
    /// Vitesse au sol (animation de marche).
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
    Player {
        pseudo: String,
        pos: [f32; 3],
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
        pos: [f32; 3],
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
        at: [f32; 3],
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
        from: [f32; 3],
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
        pos: [f32; 3],
        dir: [f32; 3],
        seed: u32,
    },
    /// (v229) Foudre publiée par l'arbitre aux joueurs proches (et à celui qui l'a
    /// demandée) : elle part chez chacun à la réception. Rien n'en est gardé (un joueur qui
    /// arrive ensuite ne la revoit pas). `by`, `seq` : l'OP dont la demande est acceptée.
    Thor {
        chunk: [i32; 2],
        up: bool,
        pos: [f32; 3],
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
        pos: [f32; 3],
        to: [f32; 3],
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
        pos: [f32; 3],
        to: [f32; 3],
        seed: u32,
        by: String,
        seq: u32,
    },
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
    if !finite
        || permit.len() > 128
        || pos[0].abs() > 3.0e7
        || pos[2].abs() > 3.0e7
        || !(-64.0..=4096.0).contains(&pos[1])
    {
        return false;
    }
    let at = [
        (pos[0].floor() as i32).div_euclid(16),
        (pos[2].floor() as i32).div_euclid(16),
    ];
    let d = [to[0] - pos[0], to[1] - pos[1], to[2] - pos[2]];
    let len2 = to.iter().map(|v| v * v).sum::<f32>();
    at == *chunk
        && match *act {
            ZEUS_STRIKE => true,
            ZEUS_SKY => (0.9..=1.1).contains(&len2) && to[1] > 0.05,
            ZEUS_ARC => d.iter().map(|v| v * v).sum::<f32>() <= ZEUS_ARC_MAX * ZEUS_ARC_MAX,
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
    let finite = pos.iter().chain(dir.iter()).all(|v| v.is_finite());
    if !finite || pos[0].abs() > 3.0e7 || pos[2].abs() > 3.0e7 || !(-64.0..=4096.0).contains(&pos[1])
    {
        return false;
    }
    let at = [
        (pos[0].floor() as i32).div_euclid(16),
        (pos[2].floor() as i32).div_euclid(16),
    ];
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
    /// touche un joueur (sang).
    Shot {
        who: u64,
        weapon: u8,
        pos: [f32; 3],
        hits: Vec<[f32; 3]>,
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
}

pub fn encode<T: Serialize>(m: &T) -> Vec<u8> {
    postcard::to_allocvec(m).unwrap_or_default()
}

pub fn decode<'a, T: Deserialize<'a>>(b: &'a [u8]) -> Option<T> {
    postcard::from_bytes(b).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

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
