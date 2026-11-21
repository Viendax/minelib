//! (v209) Adversaires repérés sur la minimap (mode FPS, solo et en ligne).
//!
//! Règles demandées par Viendax : par défaut, aucun adversaire sur la minimap. Un
//! adversaire y apparaît, pour toute l'équipe, dès qu'un de ses joueurs en vie le voit :
//! - dans son champ de vision ;
//! - pas derrière des blocs pleins (les mêmes que pour les bots : vitres et panneaux fins
//!   laissent voir) ;
//! - pas derrière la partie opaque d'une fumée ;
//! - et l'écran de celui qui regarde n'est pas tout blanc (flash).
//!
//! Tant qu'il est vu, son point suit sa position ; quand plus personne ne le voit, le point
//! reste à la dernière position vue pendant 3,5 s, puis disparaît.
//!
//! Tout est calculé ici, chez chaque joueur, avec ce qu'il connaît déjà (positions et
//! regards de tous, fumées, flashs qui éclatent) : rien de plus à envoyer en ligne.

use std::collections::HashMap;

use glam::Vec3;
use minelib_world::World;

/// Durée (s) pendant laquelle la dernière position vue reste sur la minimap.
pub const HOLD: f32 = 3.5;

/// Demi-angle du champ de vision pris pour « voir » (degrés) : un peu plus que la moitié
/// du champ horizontal d'un écran large.
const VIEW_HALF_ANGLE: f32 = 60.0;

/// Recalculé au plus 20 fois par seconde (assez pour une minimap).
const PERIOD: f32 = 0.05;

/// Un joueur de l'équipe qui regarde.
pub struct Observer<'a> {
    pub pseudo: &'a str,
    pub eye: Vec3,
    pub look: Vec3,
    /// Pour le joueur de ce PC : son blanc de flash (`Fps::blind`) ; pour les autres :
    /// celui tenu ici (`Spotting::flash`).
    pub own_blind: Option<f32>,
}

/// Un adversaire en vie.
pub struct Target<'a> {
    pub pseudo: &'a str,
    pub feet: Vec3,
    pub crouch: f32,
}

#[derive(Default)]
pub struct Spotting {
    /// Dernière position vue de chaque adversaire, et quand (horloge `time`).
    seen: HashMap<String, (Vec3, f32)>,
    /// Blanc de flash des coéquipiers (secondes, comme `Fps::blind` : écran tout blanc à
    /// partir de 1).
    blind: HashMap<String, f32>,
    time: f32,
    next: f32,
}

impl Spotting {
    /// Nouvelle manche, autre partie : plus rien de repéré.
    pub fn clear(&mut self) {
        self.seen.clear();
        self.blind.clear();
    }

    /// Une flash éclate en `at` : chaque coéquipier en est aveuglé comme le joueur de ce PC
    /// le serait à sa place (même formule que `equip`).
    pub fn flash(&mut self, at: Vec3, world: &World, observers: &[Observer]) {
        for o in observers.iter().filter(|o| o.own_blind.is_none()) {
            let f = crate::equip::flash_strength(world, at, o.eye, o.look);
            if f > 0.0 {
                let b = self.blind.entry(o.pseudo.to_string()).or_insert(0.0);
                *b = b.max(f * crate::equip::FLASH_BLIND);
            }
        }
    }

    /// Avance l'horloge et regarde qui voit qui.
    pub fn update(
        &mut self,
        dt: f32,
        world: &World,
        observers: &[Observer],
        targets: &[Target],
        smokes: &[(Vec3, f32)],
    ) {
        self.time += dt;
        for b in self.blind.values_mut() {
            *b = (*b - dt).max(0.0);
        }
        self.blind.retain(|_, b| *b > 0.0);
        if self.time < self.next {
            return;
        }
        self.next = self.time + PERIOD;
        let cos_view = VIEW_HALF_ANGLE.to_radians().cos();
        for t in targets {
            // Tête, torse, genoux : vu dès qu'un des trois l'est.
            let height = 1.8 - 0.35 * t.crouch;
            let points = [height - 0.2, height * 0.6, 0.45].map(|y| t.feet + Vec3::Y * y);
            let spotted = observers.iter().any(|o| {
                let blind = o
                    .own_blind
                    .unwrap_or_else(|| self.blind.get(o.pseudo).copied().unwrap_or(0.0));
                if blind >= 1.0 {
                    return false;
                }
                points.iter().any(|&p| {
                    let d = p - o.eye;
                    let len = d.length();
                    if len < 1e-3 {
                        return true;
                    }
                    d.dot(o.look) / len >= cos_view
                        && crate::bots::clear_sight(world, o.eye, p)
                        && !crate::bots::smoked(smokes, o.eye, p)
                })
            });
            if spotted {
                self.seen.insert(t.pseudo.to_string(), (t.feet, self.time));
            }
        }
        let now = self.time;
        self.seen.retain(|_, (_, at)| now - *at <= HOLD);
    }

    /// Adversaires à montrer sur la minimap : pseudo et dernière position vue.
    pub fn shown(&self) -> impl Iterator<Item = (&str, Vec3)> + '_ {
        self.seen.iter().map(|(k, (p, _))| (k.as_str(), *p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minelib_world::BlockId;

    fn flat() -> World {
        let mut w = World::new(1);
        // (Un bloc ne se pose que dans un chunk chargé.)
        for cx in -2..2 {
            for cz in -2..2 {
                w.insert_empty(minelib_world::ChunkPos { x: cx, z: cz });
            }
        }
        for x in -20..20 {
            for z in -20..20 {
                w.set(x, 63, z, BlockId::STONE);
            }
        }
        w
    }

    #[test]
    fn an_enemy_is_spotted_only_in_sight_and_kept_three_seconds_and_a_half() {
        let mut w = flat();
        let me = |look: Vec3| Observer {
            pseudo: "moi",
            eye: Vec3::new(0.5, 65.62, 0.5),
            look,
            own_blind: Some(0.0),
        };
        let enemy = Target {
            pseudo: "bot",
            feet: Vec3::new(10.5, 64.0, 0.5),
            crouch: 0.0,
        };
        let mut s = Spotting::default();
        // Dos tourné : pas vu.
        s.update(
            0.1,
            &w,
            &[me(Vec3::NEG_X)],
            std::slice::from_ref(&enemy),
            &[],
        );
        assert_eq!(s.shown().count(), 0);
        // Face à lui : vu.
        s.update(0.1, &w, &[me(Vec3::X)], std::slice::from_ref(&enemy), &[]);
        assert_eq!(s.shown().count(), 1);
        // Un mur plein entre les deux : la dernière position reste 3,5 s, puis plus rien.
        for y in 64..68 {
            for z in -3..4 {
                w.set(5, y, z, BlockId::STONE);
            }
        }
        s.update(3.0, &w, &[me(Vec3::X)], std::slice::from_ref(&enemy), &[]);
        assert_eq!(s.shown().count(), 1, "encore là au bout de 3 s");
        s.update(0.6, &w, &[me(Vec3::X)], std::slice::from_ref(&enemy), &[]);
        assert_eq!(s.shown().count(), 0, "parti après 3,5 s");
    }

    #[test]
    fn a_smoke_or_a_white_screen_hides_the_enemy() {
        let w = flat();
        let enemy = Target {
            pseudo: "bot",
            feet: Vec3::new(10.5, 64.0, 0.5),
            crouch: 0.0,
        };
        let eye = Vec3::new(0.5, 65.62, 0.5);
        let mut s = Spotting::default();
        let smoke = [(Vec3::new(5.5, 65.0, 0.5), 3.0)];
        let me = Observer {
            pseudo: "moi",
            eye,
            look: Vec3::X,
            own_blind: Some(0.0),
        };
        s.update(0.1, &w, &[me], std::slice::from_ref(&enemy), &smoke);
        assert_eq!(s.shown().count(), 0, "derrière la fumée");
        let blinded = Observer {
            pseudo: "moi",
            eye,
            look: Vec3::X,
            own_blind: Some(1.5),
        };
        s.update(0.1, &w, &[blinded], std::slice::from_ref(&enemy), &[]);
        assert_eq!(s.shown().count(), 0, "écran tout blanc");
        let half = Observer {
            pseudo: "moi",
            eye,
            look: Vec3::X,
            own_blind: Some(0.6),
        };
        s.update(0.1, &w, &[half], std::slice::from_ref(&enemy), &[]);
        assert_eq!(s.shown().count(), 1, "pas tout blanc : vu");
    }
}
