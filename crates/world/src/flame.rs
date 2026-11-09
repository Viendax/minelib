//! Couleurs de flamme (« test de flamme ») selon l'élément chimique ajouté à la torche.
//!
//! Chaque sel, chauffé dans la flamme, émet les raies ou bandes caractéristiques de
//! son élément. Les couleurs RVB ci-dessous approchent la teinte perçue.
//!
//! **Durée de la couleur.** Une torche colorée contient 5 g de sel. La vitesse à
//! laquelle la flamme le consomme dépend surtout de sa volatilité : plus la
//! température d'ébullition (ou de décomposition/sublimation) est basse, plus il
//! part vite. Modèle de jeu, simple et assumé :
//!
//! ```text
//! débit (g/min) = 0,6 × exp((1400 − T) / 500)   borné à [0,25 ; 3]
//! durée = 5 g / débit
//! ```
//! avec T en °C. Les ordres de grandeur sont plausibles pour une petite flamme,
//! mais ce n'est pas une mesure : la vraie durée dépend de la mèche et de la flamme.

/// Masse de sel dans une torche colorée (g).
pub const CHARGE_G: f32 = 5.0;

/// Flamme d'une torche ordinaire (bois) : incandescence de suie + traces de sodium.
pub const WOOD_FLAME: [u8; 3] = [255, 150, 55];

#[derive(Debug, Clone, Copy)]
pub struct Chemical {
    pub element: &'static str,
    pub symbol: &'static str,
    /// Nom du composé, avec l'article (« chlorure de lithium »).
    pub salt: &'static str,
    pub formula: &'static str,
    pub color_name: &'static str,
    /// Raies ou bandes d'émission principales.
    pub emission: &'static str,
    /// Couleur perçue (sRGB).
    pub rgb: [u8; 3],
    /// Température d'ébullition, de sublimation ou de décomposition (°C).
    pub volatility_c: f32,
}

impl Chemical {
    /// Débit de consommation du sel par la flamme (g/min), modèle de jeu.
    pub fn burn_rate_g_per_min(&self) -> f32 {
        (0.6 * ((1400.0 - self.volatility_c) / 500.0).exp()).clamp(0.25, 3.0)
    }

    /// Durée pendant laquelle la flamme garde cette couleur (s).
    pub fn duration_s(&self) -> f32 {
        CHARGE_G / self.burn_rate_g_per_min() * 60.0
    }

    /// Durée de couleur réellement utilisée en jeu (survie). Réglage de test :
    /// [`TEST_BURN_S`] remplace la durée physique tant qu'il est défini.
    pub fn burn_time_s(&self) -> f32 {
        TEST_BURN_S.unwrap_or_else(|| self.duration_s())
    }
}

/// Test : une torche colorée posée en survie repasse en flamme normale après 10 s.
/// Mettre `None` pour revenir aux durées physiques.
pub const TEST_BURN_S: Option<f32> = None;

pub const CHEMICALS: [Chemical; 13] = [
    Chemical {
        element: "Lithium",
        symbol: "Li",
        salt: "chlorure de lithium",
        formula: "LiCl",
        color_name: "rouge carmin",
        emission: "raie Li 670,8 nm",
        rgb: [215, 20, 55],
        volatility_c: 1382.0,
    },
    Chemical {
        element: "Strontium",
        symbol: "Sr",
        salt: "chlorure de strontium",
        formula: "SrCl2",
        color_name: "rouge écarlate",
        emission: "bandes SrOH 606 nm, SrCl 636-674 nm",
        rgb: [255, 28, 22],
        volatility_c: 1250.0,
    },
    Chemical {
        element: "Calcium",
        symbol: "Ca",
        salt: "chlorure de calcium",
        formula: "CaCl2",
        color_name: "rouge orangé",
        emission: "bandes CaOH 554 et 622 nm",
        rgb: [255, 85, 25],
        volatility_c: 1935.0,
    },
    Chemical {
        element: "Sodium",
        symbol: "Na",
        salt: "chlorure de sodium",
        formula: "NaCl",
        color_name: "jaune intense",
        emission: "doublet Na 589,0 / 589,6 nm",
        rgb: [255, 205, 25],
        volatility_c: 1465.0,
    },
    Chemical {
        element: "Baryum",
        symbol: "Ba",
        salt: "chlorure de baryum",
        formula: "BaCl2",
        color_name: "vert pomme",
        emission: "bandes BaCl 513-532 nm",
        rgb: [165, 255, 80],
        volatility_c: 1560.0,
    },
    Chemical {
        element: "Bore",
        symbol: "B",
        salt: "acide borique",
        formula: "H3BO3",
        color_name: "vert vif",
        emission: "bandes BO2 518 et 548 nm",
        rgb: [40, 255, 60],
        volatility_c: 300.0,
    },
    Chemical {
        element: "Cuivre",
        symbol: "Cu",
        salt: "sulfate de cuivre(II)",
        formula: "CuSO4",
        color_name: "vert émeraude",
        emission: "bandes CuOH 525-555 nm",
        rgb: [30, 220, 130],
        volatility_c: 1800.0,
    },
    Chemical {
        element: "Cuivre",
        symbol: "Cu",
        salt: "chlorure de cuivre(II)",
        formula: "CuCl2",
        color_name: "bleu azur",
        emission: "bandes CuCl 420-460 nm",
        rgb: [30, 140, 255],
        volatility_c: 993.0,
    },
    Chemical {
        element: "Zinc",
        symbol: "Zn",
        salt: "chlorure de zinc",
        formula: "ZnCl2",
        color_name: "bleu-vert pâle",
        emission: "émission faible, bleu-vert",
        rgb: [140, 225, 210],
        volatility_c: 732.0,
    },
    Chemical {
        element: "Indium",
        symbol: "In",
        salt: "chlorure d'indium",
        formula: "InCl3",
        color_name: "bleu indigo",
        emission: "raies In 410,2 et 451,1 nm",
        rgb: [55, 60, 235],
        volatility_c: 800.0,
    },
    Chemical {
        element: "Césium",
        symbol: "Cs",
        salt: "chlorure de césium",
        formula: "CsCl",
        color_name: "bleu-violet",
        emission: "raies Cs 455,5 et 459,3 nm",
        rgb: [105, 85, 255],
        volatility_c: 1297.0,
    },
    Chemical {
        element: "Potassium",
        symbol: "K",
        salt: "chlorure de potassium",
        formula: "KCl",
        color_name: "lilas",
        emission: "raies K 404,4 nm et 766,5 / 769,9 nm",
        rgb: [195, 135, 255],
        volatility_c: 1420.0,
    },
    Chemical {
        element: "Rubidium",
        symbol: "Rb",
        salt: "chlorure de rubidium",
        formula: "RbCl",
        color_name: "rouge-violet",
        emission: "raies Rb 420,2 nm et 780,0 / 794,8 nm",
        rgb: [205, 55, 165],
        volatility_c: 1390.0,
    },
];

/// « 8 min 02 s », « 45 s ».
pub fn format_duration(secs: f32) -> String {
    let s = secs.max(0.0).round() as u32;
    if s >= 60 {
        format!("{} min {:02} s", s / 60, s % 60)
    } else {
        format!("{s} s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_are_reasonable() {
        for c in CHEMICALS {
            let d = c.duration_s();
            assert!((60.0..=1300.0).contains(&d), "{} : {d} s", c.salt);
        }
        // Plus volatil = consommé plus vite.
        let zn = CHEMICALS.iter().find(|c| c.formula == "ZnCl2").unwrap();
        let ca = CHEMICALS.iter().find(|c| c.formula == "CaCl2").unwrap();
        assert!(zn.duration_s() < ca.duration_s());
    }

    #[test]
    fn format() {
        assert_eq!(format_duration(482.4), "8 min 02 s");
        assert_eq!(format_duration(45.0), "45 s");
    }
}
