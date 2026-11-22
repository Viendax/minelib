//! (v241) La Foudre de Zeus, objet légendaire. Règles de Viendax (22 novembre, notées dans
//! le projet : `claude/foudre-de-zeus.md`) :
//!
//! - **un seul éclair par carte**, pris une seule fois dans le menu créatif (ou par
//!   `/give`), et qu'aucun moyen ne supprime : ni la poubelle du créatif, ni le vide, ni le
//!   trop-plein d'objets au sol ;
//! - il garde **jusqu'à 10 charges**. Une charge vient de deux coups donnés vers le ciel, ou
//!   d'un éclair qui court dans les nuages au-dessus de celui qui le porte ;
//! - **chargé**, un coup fait tomber la foudre du ciel sur ce qu'on vise, jusqu'à 128 blocs,
//!   et prend une charge ;
//! - **sans charge**, un arc part de sa pointe jusqu'à 8 blocs, sans passer par le ciel, et
//!   blesse 2,5 fois moins. Il part même si l'on ne vise personne (« même si je ne touche
//!   pas un joueur, le petit éclair à 8 blocs part quand même »).
//!
//! Ici : l'état gardé avec la carte et ce que fait un coup. Le reste est dans `session.rs`
//! (le clic), `inventory.rs` (le menu créatif, la poubelle), `storm.rs` (les éclairs des
//! nuages) et `drops.rs` (l'objet posé au sol).

use std::sync::atomic::{AtomicU8, Ordering};

/// Charges qu'il garde au plus.
pub const MAX_CHARGES: u8 = 10;
/// Coups vers le ciel pour une charge.
pub const SKY_HITS: u8 = 2;
/// Sans charge : portée de l'arc qui part de sa pointe (blocs).
pub const ARC_REACH: f32 = 8.0;
/// Chargé : portée de la foudre du ciel (blocs).
pub const STRIKE_REACH: f32 = 128.0;
/// Dégâts de l'arc sans charge : 2,5 fois moins que la foudre du ciel (5).
pub const ARC_DAMAGE: u8 = 2;
/// Un éclair des nuages charge celui qui le porte à moins de tant de blocs (à plat).
pub const CHARGE_REACH: f32 = 160.0;

/// Charges à montrer sur l'icône de l'objet (à la place du nombre de la pile) et dans son
/// info-bulle ; `HIDDEN` : rien (sur un serveur de construction, où ces règles ne sont pas
/// encore tenues par le relais).
static SHOWN: AtomicU8 = AtomicU8::new(HIDDEN);
const HIDDEN: u8 = u8::MAX;

/// La partie dit à l'interface combien de charges montrer (`None` : rien).
pub fn show(charges: Option<u8>) {
    SHOWN.store(charges.unwrap_or(HIDDEN), Ordering::Relaxed);
}

/// Charges à montrer sur l'objet (`None` : rien).
pub fn shown() -> Option<u8> {
    match SHOWN.load(Ordering::Relaxed) {
        HIDDEN => None,
        n => Some(n),
    }
}

/// Ce que fait un coup de l'éclair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blow {
    /// Vers le ciel : la foudre monte de sa pointe (sans charge aussi) et le recharge.
    Sky,
    /// Chargé : la foudre tombe du ciel sur ce qu'on vise.
    Strike,
    /// Sans charge : un arc part de sa pointe, jusqu'à ce qu'on vise ou 8 blocs plus loin.
    Arc,
    /// Chargé, mais rien à viser à moins de 128 blocs : rien ne part.
    TooFar,
}

/// L'éclair de la carte : pris ou non, et ses charges. Gardé avec la carte (`level.txt`,
/// clé `foudre`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Legend {
    /// Il a été pris (menu créatif, `/give`) : il existe quelque part sur la carte, et
    /// personne ne peut en prendre un autre.
    pub taken: bool,
    pub charges: u8,
    /// Coups vers le ciel depuis la dernière charge gagnée ainsi.
    pub sky_hits: u8,
}

impl Legend {
    /// Ce que fait un coup. `sky` : on vise le ciel (rien à moins de `STRIKE_REACH` blocs,
    /// le regard au-dessus de l'horizon) ; `target` : il y a un bloc, un joueur ou un PNJ
    /// à viser à moins de `STRIKE_REACH` blocs.
    pub fn blow(&self, sky: bool, target: bool) -> Blow {
        if sky {
            Blow::Sky
        } else if self.charges == 0 {
            Blow::Arc
        } else if target {
            Blow::Strike
        } else {
            Blow::TooFar
        }
    }

    /// Un coup vers le ciel est parti. Vrai : il vient de gagner une charge (au deuxième).
    /// Plein, les coups ne comptent pas.
    pub fn sky_hit(&mut self) -> bool {
        if self.charges >= MAX_CHARGES {
            self.sky_hits = 0;
            return false;
        }
        self.sky_hits += 1;
        if self.sky_hits < SKY_HITS {
            return false;
        }
        self.sky_hits = 0;
        self.charges += 1;
        true
    }

    /// Un éclair a couru dans les nuages au-dessus de celui qui le porte. Vrai : une charge
    /// de plus.
    pub fn charge(&mut self) -> bool {
        if self.charges >= MAX_CHARGES {
            return false;
        }
        self.charges += 1;
        true
    }

    /// La foudre du ciel part : une charge de moins. Faux : il n'en avait pas.
    pub fn spend(&mut self) -> bool {
        if self.charges == 0 {
            return false;
        }
        self.charges -= 1;
        true
    }

    /// Pour la sauvegarde de la carte : « pris;charges;coups vers le ciel ».
    pub fn save(&self) -> String {
        format!("{};{};{}", u8::from(self.taken), self.charges, self.sky_hits)
    }

    /// Relit ce qu'a écrit `save` (vide ou illisible : pas encore pris, sans charge).
    pub fn load(s: &str) -> Self {
        let mut f = s.split(';').map(|v| v.trim().parse::<u8>().ok());
        let taken = f.next().flatten().is_some_and(|v| v != 0);
        let charges = f.next().flatten().unwrap_or(0).min(MAX_CHARGES);
        let sky_hits = f.next().flatten().unwrap_or(0).min(SKY_HITS - 1);
        Self {
            taken,
            charges: if taken { charges } else { 0 },
            sky_hits: if taken { sky_hits } else { 0 },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deux coups vers le ciel pour une charge, dix charges au plus ; un éclair des nuages
    /// en donne une d'un coup ; la foudre du ciel en prend une.
    #[test]
    fn two_sky_hits_make_a_charge_up_to_ten() {
        let mut l = Legend {
            taken: true,
            ..Default::default()
        };
        assert!(!l.sky_hit() && l.charges == 0);
        assert!(l.sky_hit() && l.charges == 1 && l.sky_hits == 0);
        assert!(l.charge() && l.charges == 2);
        for _ in 0..40 {
            l.sky_hit();
        }
        assert_eq!((l.charges, l.sky_hits), (MAX_CHARGES, 0));
        assert!(!l.charge() && !l.sky_hit() && l.charges == MAX_CHARGES);
        assert!(l.spend() && l.charges == MAX_CHARGES - 1);
        // (Un coup vers le ciel compte de nouveau.)
        assert!(!l.sky_hit() && l.sky_hit() && l.charges == MAX_CHARGES);
        for _ in 0..MAX_CHARGES {
            assert!(l.spend());
        }
        assert!(!l.spend() && l.charges == 0);
    }

    /// Chargé : la foudre du ciel sur ce qu'on vise, même tout près ; sans charge : l'arc
    /// court, qu'on vise quelque chose ou non ; vers le ciel : toujours la foudre qui monte.
    #[test]
    fn a_blow_depends_on_the_charge_and_the_distance() {
        let empty = Legend {
            taken: true,
            ..Default::default()
        };
        let full = Legend {
            charges: 3,
            ..empty
        };
        assert_eq!(empty.blow(false, true), Blow::Arc);
        assert_eq!(empty.blow(false, false), Blow::Arc);
        assert_eq!(full.blow(false, true), Blow::Strike);
        assert_eq!(full.blow(false, false), Blow::TooFar);
        assert_eq!(empty.blow(true, false), Blow::Sky);
        assert_eq!(full.blow(true, false), Blow::Sky);
        assert!(ARC_REACH < STRIKE_REACH);
        // 2,5 fois moins de dégâts que la foudre.
        assert_eq!(
            (crate::lightning::DAMAGE as f32 / 2.5).round() as u8,
            ARC_DAMAGE
        );
    }

    /// Gardé avec la carte ; une valeur abîmée ne donne ni charge en trop ni éclair pris à
    /// tort.
    #[test]
    fn the_legend_is_saved_with_the_map() {
        let l = Legend {
            taken: true,
            charges: 7,
            sky_hits: 1,
        };
        assert_eq!(l.save(), "1;7;1");
        assert_eq!(Legend::load(&l.save()), l);
        assert_eq!(Legend::load(""), Legend::default());
        assert_eq!(Legend::load("0;9;1"), Legend::default());
        assert_eq!(Legend::load("1;200;9").charges, MAX_CHARGES);
        assert_eq!(Legend::load("1;200;9").sky_hits, SKY_HITS - 1);
        assert_eq!(
            Legend::load("1"),
            Legend {
                taken: true,
                ..Default::default()
            }
        );
    }
}
