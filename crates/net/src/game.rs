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
    /// Arme en main (indice dans la liste des armes, 255 = couteau ou rien).
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
    /// Arme lâchée (touche G).
    Drop {
        weapon: u8,
    },
    /// Ramasser une arme au sol.
    Pickup {
        id: u32,
    },
}

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
}
