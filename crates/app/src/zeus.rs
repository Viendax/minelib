//! (v241) La Foudre de Zeus, objet légendaire. Règles de Viendax (22 novembre, notées dans
//! le projet : `claude/foudre-de-zeus.md`) :
//!
//! - **un seul éclair par carte**, pris une seule fois dans le menu créatif (ou par
//!   `/give`), et qu'aucun moyen ne supprime : ni la poubelle du créatif, ni le vide, ni le
//!   trop-plein d'objets au sol ;
//! - il garde **jusqu'à 10 charges**. Une charge vient de deux coups donnés vers le ciel, ou
//!   d'un éclair qui court dans les nuages au-dessus de celui qui le porte ;
//! - **chargé**, un coup fait tomber la foudre du ciel sur ce qu'on vise et prend une
//!   charge ; (v244) sans distance maximale : « c'est comme le /thor » (128 blocs avant) ;
//! - **sans charge**, un arc part de sa pointe jusqu'à 8 blocs, sans passer par le ciel, et
//!   blesse 2,5 fois moins. Il part même si l'on ne vise personne (« même si je ne touche
//!   pas un joueur, le petit éclair à 8 blocs part quand même »). (v244) Il part toujours du
//!   bout de l'éclair et fait vraiment ses 8 blocs, ou va jusqu'au bloc visé s'il est à
//!   portée (`arc_end`).
//!
//! (v244) **En ligne**, sur un serveur de construction, c'est le relais du monde qui tient
//! tout cela (principe du projet : il valide tout) : qui a l'éclair du serveur, ses charges,
//! et le droit de donner chaque coup (`Online`, `minelib_net::proto::Request::Zeus`). Il y
//! reste avec celui qui l'a pris : il ne se pose pas, ne se range pas dans un coffre.
//!
//! Ici : l'état gardé avec la carte et ce que fait un coup. Le reste est dans `session.rs`
//! (le clic), `inventory.rs` (le menu créatif, la poubelle), `storm.rs` (les éclairs des
//! nuages) et `drops.rs` (l'objet posé au sol).

use glam::Vec3;
use std::sync::atomic::{AtomicU8, Ordering};

/// Charges qu'il garde au plus.
pub const MAX_CHARGES: u8 = minelib_net::proto::ZEUS_MAX_CHARGES;
/// Coups vers le ciel pour une charge.
pub const SKY_HITS: u8 = minelib_net::proto::ZEUS_SKY_HITS;
/// Sans charge : portée de l'arc qui part de sa pointe (blocs).
pub const ARC_REACH: f32 = 8.0;
/// Dégâts de l'arc sans charge : 2,5 fois moins que la foudre du ciel (5).
pub const ARC_DAMAGE: u8 = 2;
/// Un éclair des nuages charge celui qui le porte à moins de tant de blocs (à plat).
pub const CHARGE_REACH: f32 = minelib_net::meteo::CHARGE_REACH;

/// (v244) Où finit le petit éclair qui ne vise rien à portée : sur le rayon du regard (parti
/// de `eye` vers `dir`, de longueur 1), au point qui est à `reach` blocs de la pointe `tip`
/// de l'éclair (« le petit éclair qui part à 8 blocs doit vraiment faire les 8 blocs de
/// trajet » : mesurés de l'œil, il en faisait moins).
pub fn arc_end(eye: Vec3, dir: Vec3, tip: Vec3, reach: f32) -> Vec3 {
    let o = eye - tip;
    let b = o.dot(dir);
    let c = o.length_squared() - reach * reach;
    let t = -b + (b * b - c).max(0.0).sqrt();
    eye + dir * t.max(0.0)
}

/// (v244) Sur un serveur de construction : ce que le relais du monde dit de l'éclair du
/// serveur, et où en sont nos demandes.
#[derive(Clone, Debug, Default)]
pub struct Online {
    /// L'état a été demandé (à l'arrivée sur le serveur).
    pub asked: bool,
    /// Celui qui l'a, d'après le relais (minuscules ; vide : personne). `None` : pas encore
    /// dit.
    pub holder: Option<String>,
    /// On a demandé à le prendre (catalogue, `/give`) : réponse attendue.
    pub claiming: bool,
    /// On a dit l'avoir déjà (pris en solo sur cette carte) : une fois ; `holding` : la
    /// réponse n'est pas encore là.
    pub hold_asked: bool,
    pub holding: bool,
    /// (v247) On la rend au menu créatif (supprimée en créatif) : réponse attendue ; d'ici
    /// là, elle ne nous est pas remise.
    pub returning: bool,
}

impl Online {
    /// Est-ce `pseudo` qui l'a ?
    pub fn mine(&self, pseudo: &str) -> bool {
        self.holder
            .as_deref()
            .is_some_and(|h| !h.is_empty() && h.eq_ignore_ascii_case(pseudo))
    }

    /// Personne ne l'a (et le relais l'a dit).
    pub fn free(&self) -> bool {
        self.holder.as_deref() == Some("")
    }
}

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
    /// (v244) Pris, mais par un autre joueur du serveur de cette carte : il n'est pas ici
    /// (et on n'en prend pas un autre en solo).
    pub elsewhere: bool,
}

impl Legend {
    /// Ce que fait un coup. `sky` : on vise le ciel (comme `/thor` : aucun bloc sur le
    /// rayon du regard, tourné au-dessus de l'horizon).
    pub fn blow(&self, sky: bool) -> Blow {
        if sky {
            Blow::Sky
        } else if self.charges == 0 {
            Blow::Arc
        } else {
            Blow::Strike
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

    /// Pour la sauvegarde de la carte : « pris;charges;coups vers le ciel;ailleurs ».
    pub fn save(&self) -> String {
        format!(
            "{};{};{};{}",
            u8::from(self.taken),
            self.charges,
            self.sky_hits,
            u8::from(self.elsewhere)
        )
    }

    /// Relit ce qu'a écrit `save` (vide ou illisible : pas encore pris, sans charge).
    pub fn load(s: &str) -> Self {
        let mut f = s.split(';').map(|v| v.trim().parse::<u8>().ok());
        let taken = f.next().flatten().is_some_and(|v| v != 0);
        let charges = f.next().flatten().unwrap_or(0).min(MAX_CHARGES);
        let sky_hits = f.next().flatten().unwrap_or(0).min(SKY_HITS - 1);
        let elsewhere = taken && f.next().flatten().is_some_and(|v| v != 0);
        Self {
            taken,
            charges: if taken && !elsewhere { charges } else { 0 },
            sky_hits: if taken && !elsewhere { sky_hits } else { 0 },
            elsewhere,
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
        assert_eq!(empty.blow(false), Blow::Arc);
        assert_eq!(full.blow(false), Blow::Strike);
        assert_eq!(empty.blow(true), Blow::Sky);
        assert_eq!(full.blow(true), Blow::Sky);
        // 2,5 fois moins de dégâts que la foudre.
        assert_eq!(
            (crate::lightning::DAMAGE as f32 / 2.5).round() as u8,
            ARC_DAMAGE
        );
    }

    /// (v244) Le petit éclair qui ne vise rien fait ses 8 blocs depuis la pointe de
    /// l'éclair, et finit sur le rayon du regard.
    #[test]
    fn the_small_arc_really_travels_its_eight_blocks() {
        let eye = Vec3::new(10.0, 65.6, -4.0);
        for (dir, tip) in [
            (Vec3::new(0.0, 0.0, -1.0), Vec3::new(10.3, 65.3, -4.9)),
            (Vec3::new(0.6, 0.64, -0.48).normalize(), Vec3::new(10.4, 65.9, -4.6)),
            (Vec3::new(-0.3, -0.9, 0.2).normalize(), Vec3::new(9.8, 64.9, -3.7)),
        ] {
            let end = arc_end(eye, dir, tip, ARC_REACH);
            assert!(((end - tip).length() - ARC_REACH).abs() < 1e-3, "{end}");
            // Sur le rayon du regard, devant l'œil, plus loin que 8 blocs moins la main.
            let t = (end - eye).dot(dir);
            assert!(((end - eye) - dir * t).length() < 1e-3 && t > ARC_REACH - 1.5);
        }
    }

    /// (v244) En ligne : à qui est l'éclair, d'après le relais.
    #[test]
    fn the_relay_says_who_holds_the_bolt_online() {
        let mut o = Online::default();
        assert!(!o.free() && !o.mine("Eve"));
        o.holder = Some(String::new());
        assert!(o.free() && !o.mine("Eve") && !o.mine(""));
        o.holder = Some("eve".into());
        assert!(!o.free() && o.mine("Eve") && !o.mine("Adam"));
    }

    /// Gardé avec la carte ; une valeur abîmée ne donne ni charge en trop ni éclair pris à
    /// tort.
    #[test]
    fn the_legend_is_saved_with_the_map() {
        let l = Legend {
            taken: true,
            charges: 7,
            sky_hits: 1,
            elsewhere: false,
        };
        assert_eq!(l.save(), "1;7;1;0");
        assert_eq!(Legend::load(&l.save()), l);
        // (v244) Une sauvegarde d'avant (sans le dernier champ) se relit pareil ; pris par
        // un autre joueur du serveur : sans charge ici.
        assert_eq!(Legend::load("1;7;1"), l);
        let away = Legend::load("1;7;1;1");
        assert!(away.taken && away.elsewhere && away.charges == 0 && away.sky_hits == 0);
        assert_eq!(Legend::load(&away.save()), away);
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
