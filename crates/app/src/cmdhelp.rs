//! (v251) Aide des commandes, au-dessus de la saisie du tchat pendant qu'on tape. Demande de
//! Viendax (23 novembre) : « rends plus compréhensibles les informations sur l'utilisation et
//! les arguments des commandes quand on tape le début de celle-ci (le /fps par exemple, ce
//! n'est pas très clair) ; qu'elle affiche aussi les informations enregistrées qui les
//! concernent ».
//!
//! - Tant que le nom tapé en commence plusieurs : une ligne par commande (sa forme et ce
//!   qu'elle fait).
//! - Dès qu'il n'en reste qu'une (ou que le nom est entier) : chacune de ses formes sur sa
//!   ligne, avec ce qu'elle fait, puis ce qui est enregistré qui la concerne (donné par la
//!   partie : `Session::command_state`).

/// Une commande : ses noms (le premier est montré), ce qu'elle fait, et ses formes (ce qu'on
/// tape, ce que ça fait).
pub struct Help {
    pub names: &'static [&'static str],
    pub what: &'static str,
    pub forms: &'static [(&'static str, &'static str)],
}

pub const HELP: &[Help] = &[
    Help {
        names: &["fps"],
        what: "Réglages de la carte FPS de ce monde (hors partie)",
        forms: &[
            ("/fps info", "ce qui est réglé"),
            (
                "/fps set spawn ct | spawn t",
                "zone de départ d'une équipe : la sélection faite à la baguette (//wand)",
            ),
            ("/fps set site a | site b | site c", "zone d'un site de bombe (sélection)"),
            ("/fps clear <zone>", "efface une zone (spawn ct, site b…)"),
            (
                "/fps set maxplayer <2|4|6|8|10|20>",
                "joueurs de la carte : 1v1, 2v2, 3v3, 4v4, 5v5 (le seul classé) ou 10v10",
            ),
            (
                "/fps set door",
                "la porte visée, ou celles de la sélection : elle s'ouvre en partie",
            ),
            ("/fps clear door [all]", "la retire (all : toutes)"),
            ("/fps set door on|off", "toutes les portes s'ouvrent en partie, ou seulement celles posées"),
            ("/fps set doorsync on|off", "doubles portes ensemble ou séparées"),
            ("/fps set time <heure | default>", "heure de la carte en partie"),
            (
                "/fps showelements on|off",
                "portes (vert clair, doubles : vert foncé), sites (rouge), départs CT (bleu) et T \
                 (jaune) vus à travers les murs",
            ),
        ],
    },
    Help {
        names: &["bot", "bots"],
        what: "Bots de la partie FPS (solo ; serveur de construction : OP)",
        forms: &[
            ("/bot", "lance une partie, ou la complète sans la recommencer"),
            ("/bot maxbot <2-20>", "joueurs de la partie, toi compris (gardé)"),
            ("/bot facile | normal | difficile", "leur niveau (gardé)"),
            ("/bot off", "sans bots (serveur : ferme la partie)"),
        ],
    },
    Help {
        names: &["start"],
        what: "Lance la partie FPS (ou la fait commencer si elle attend)",
        forms: &[("/start", "")],
    },
    Help {
        names: &["restart"],
        what: "Recommence la partie FPS du début",
        forms: &[("/restart", "")],
    },
    Help {
        names: &["gm", "gamemode"],
        what: "Mode de jeu",
        forms: &[
            ("/gm 0 | 1 | 2 | 3", "survie, créatif, FPS, spectateur (les noms marchent aussi)"),
            ("/gm <mode> <pseudo>", "celui d'un autre joueur (OP d'un serveur)"),
        ],
    },
    Help {
        names: &["defaultgamemode", "dgm"],
        what: "Mode de jeu des nouveaux venus sur ce serveur (OP)",
        forms: &[("/defaultgamemode <mode>", ""), ("/defaultgamemode off", "chacun garde le sien")],
    },
    Help {
        names: &["fly"],
        what: "Vol permis en survie (deux appuis sur Espace) ; aucune chute ne blesse",
        forms: &[("/fly [on|off]", "")],
    },
    Help {
        names: &["heal"],
        what: "Rend tous les cœurs",
        forms: &[("/heal", "")],
    },
    Help {
        names: &["tp", "teleport"],
        what: "Téléporter (en ligne : OP)",
        forms: &[
            ("/tp <x y z>", "toi, à ces coordonnées (~ : par rapport à toi)"),
            ("/tp <pseudo>", "toi, jusqu'à ce joueur"),
            ("/tp <pseudo> <x y z | pseudo>", "un autre joueur"),
        ],
    },
    Help {
        names: &["setspawn", "spawnpoint"],
        what: "Point d'apparition là où tu es (en ligne, OP : pour qui arrive)",
        forms: &[("/setspawn", "")],
    },
    Help {
        names: &["worldname", "nommonde"],
        what: "Renomme le monde",
        forms: &[("/worldname <nom>", "")],
    },
    Help {
        names: &["timeset", "heure"],
        what: "Heure du jour (en ligne, OP : pour tout le serveur)",
        forms: &[
            ("/timeset <heure>", "11, 11:20, 11.20 ou 11h20"),
            ("/timeset default", "le soleil de Dust2, immobile"),
        ],
    },
    Help {
        names: &["timefix"],
        what: "Arrête ou relance le temps qui passe",
        forms: &[("/timefix on|off", "")],
    },
    Help {
        names: &["weather", "meteo", "météo"],
        what: "Le temps qu'il fait (en ligne, OP : pour tout le serveur)",
        forms: &[
            ("/weather clear | rain | storm", "beau temps, pluie, orage"),
            ("/weather fix [on|off]", "le fige (ou le laisse changer)"),
        ],
    },
    Help {
        names: &["thor", "foudre"],
        what: "La foudre tombe là où tu vises (en ligne : OP)",
        forms: &[("/thor", "")],
    },
    Help {
        names: &["give"],
        what: "Donne un objet",
        forms: &[
            ("/give <bloc> [nombre]", "ex. /give mirror, /give iron_block"),
            ("/give foudre | lampe | ecran", "Foudre de Zeus, lampe torche, écran vidéo"),
            ("/give petit_ecran | grand_ecran | ecran_geant", "autres tailles d'écran"),
            ("/give tête <pseudo> [nombre]", "la tête d'un joueur, avec son skin"),
        ],
    },
    Help {
        names: &["whitelist", "wl"],
        what: "Liste blanche du serveur de construction (OP) ; un monde ouvert en ligne est fermé par défaut",
        forms: &[
            ("/whitelist on | off", "fermé (seulement OP, liste, amis invités) ou ouvert à tous"),
            ("/whitelist add | remove <pseudo>", "ajoute ou retire un joueur"),
            ("/whitelist list", "état et pseudos de la liste"),
        ],
    },
    Help {
        names: &["op"],
        what: "Rend un joueur OP de ce serveur (un toutes les 30 s au plus)",
        forms: &[("/op <pseudo>", ""), ("/op", "la liste des OP")],
    },
    Help {
        names: &["deop"],
        what: "Retire l'OP (le créateur seulement)",
        forms: &[("/deop <pseudo>", "")],
    },
    Help {
        names: &["ban"],
        what: "Bannit un joueur de ce serveur (OP)",
        forms: &[("/ban <pseudo> [raison]", "")],
    },
    Help {
        names: &["unban", "pardon"],
        what: "Lève un bannissement (OP)",
        forms: &[("/unban <pseudo>", "")],
    },
    Help {
        names: &["kill"],
        what: "Tue un joueur du serveur (OP)",
        forms: &[("/kill <pseudo>", "")],
    },
    Help {
        names: &["lumieres", "lumières", "lights"],
        what: "Éditeur de lumières (touche L)",
        forms: &[("/lumieres", "l'ouvre"), ("/lumieres effacer", "retire toutes les lumières")],
    },
    Help {
        names: &["npc"],
        what: "Essais de PNJ",
        forms: &[
            ("/npc stresstest on|off", "foule d'essai (4 000 PNJ ; Pistache s'y cache)"),
            ("/npc reflets on|off", "joueurs reflétés dans l'eau"),
        ],
    },
    Help {
        names: &["yt"],
        what: "YouTube sur les écrans vidéo",
        forms: &[("/yt [recherche | adresse]", "")],
    },
    Help {
        names: &["fusion"],
        what: "Fusionne les faces de blocs voisines (moins de triangles)",
        forms: &[("/fusion [on|off]", "")],
    },
    Help {
        names: &["ami", "friend"],
        what: "Amis",
        forms: &[("/ami <pseudo>", "l'ajoute (demande d'ami)"), ("/ami retirer <pseudo>", "")],
    },
    Help {
        names: &["amis", "friends"],
        what: "La liste de tes amis",
        forms: &[("/amis", "")],
    },
    Help {
        names: &["report", "signaler"],
        what: "Signale un joueur",
        forms: &[("/report <pseudo>", ""), ("/unreport <pseudo>", "annule")],
    },
    Help {
        names: &["deleteworld", "supprimermonde"],
        what: "Supprime ce monde en ligne (son créateur seulement)",
        forms: &[("/deleteworld", "")],
    },
    Help {
        names: &["reportmap", "signalercarte"],
        what: "Signale la carte de la partie FPS en cours",
        forms: &[("/reportmap", "")],
    },
    // (v346) Montrée seulement au compte qui invite (`FORWARDER`).
    Help {
        names: &[FORWARDER, "forwarders", "passeurs", "passeur"],
        what: "Passeurs sans joueur de confiance (forwarders) de ce relais",
        forms: &[
            (
                "/forwarder invite [n]",
                "invitation pour n passeurs sans joueur (4), valable 7 jours : copiée",
            ),
            ("/forwarder list", "les passeurs de confiance, et les invitations ouvertes"),
            ("/forwarder remove <pseudo>", "lui retire sa confiance"),
        ],
    },
    Help {
        names: &["help", "aide", "?"],
        what: "Toutes les commandes",
        forms: &[("/help", "")],
    },
];

/// Commandes dont un nom commence par `word` (sans « / », sans casse) ; celle dont un nom
/// est `word` exactement, d'abord.
pub fn matching(word: &str) -> Vec<&'static Help> {
    let w = word.to_lowercase();
    let mut out: Vec<&'static Help> = HELP
        .iter()
        .filter(|h| h.names.iter().any(|n| n.starts_with(&w)))
        .collect();
    out.sort_by_key(|h| !h.names.contains(&w.as_str()));
    out
}

/// Couleurs des lignes de l'aide.
pub const TITLE: [f32; 4] = [1.0, 0.86, 0.35, 1.0];
pub const FORM: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
pub const NOTE: [f32; 4] = [0.68, 0.7, 0.74, 1.0];
pub const SAVED: [f32; 4] = [0.55, 0.9, 0.6, 1.0];

/// (v346) Premier nom de /forwarder : l'aide ne la montre qu'au compte qui invite des passeurs
/// (`proto::can_invite_forwarders`, le même que celui qui certifie les cartes).
pub const FORWARDER: &str = "forwarder";

/// (v340) Formes de /fps montrées seulement au compte qui certifie les cartes
/// (`proto::CERTIFIER`) : pour les autres, elles n'existent pas.
pub const CERTIFIER_FORMS: &[(&str, &str)] = &[(
    "/fps certif true|false",
    "certifie la carte de ce monde : en tête de la liste FPS de tous (ou plus)",
)];

/// Lignes de l'aide pour la saisie `input` (qui commence par « / »), au plus `max` ;
/// `state` : ce qui est enregistré pour une commande (par son premier nom) ; (v340)
/// `certifier` : le joueur est le compte qui certifie les cartes (`CERTIFIER_FORMS`).
pub fn lines(
    input: &str,
    max: usize,
    certifier: bool,
    mut state: impl FnMut(&str) -> Vec<String>,
) -> Vec<(String, [f32; 4])> {
    let Some(rest) = input.strip_prefix('/') else {
        return Vec::new();
    };
    // WorldEdit (//set…) : son aide est //help.
    if rest.starts_with('/') {
        return Vec::new();
    }
    let word = rest.split_whitespace().next().unwrap_or("");
    let mut found = matching(word);
    // (v346) /forwarder : pour les autres, elle n'existe pas.
    found.retain(|h| certifier || h.names[0] != FORWARDER);
    let low = word.to_lowercase();
    let exact = found
        .first()
        .copied()
        .filter(|h| h.names.contains(&low.as_str()));
    let single = exact.or_else(|| (found.len() == 1).then(|| found[0]));
    let mut out = Vec::new();
    if found.is_empty() {
        if !word.is_empty() {
            out.push((format!("Aucune commande /{word}… (/help : toutes)"), NOTE));
        }
    } else if let Some(h) = single {
        out.push((format!("/{} : {}", h.names[0], h.what), TITLE));
        let extra: &[(&str, &str)] = if certifier && h.names[0] == "fps" {
            CERTIFIER_FORMS
        } else {
            &[]
        };
        for (form, what) in h.forms.iter().chain(extra) {
            if what.is_empty() {
                out.push((format!("  {form}"), FORM));
            } else {
                out.push((format!("  {form}  —  {what}"), FORM));
            }
        }
        let saved = state(h.names[0]);
        if !saved.is_empty() {
            out.push(("Enregistré :".to_string(), SAVED));
            out.extend(saved.into_iter().map(|l| (format!("  {l}"), SAVED)));
        }
    } else {
        for h in &found {
            out.push((format!("/{}  —  {}", h.names[0], h.what), FORM));
        }
    }
    if out.len() > max {
        let more = out.len() - max + 1;
        out.truncate(max - 1);
        out.push((format!("… et {more} lignes de plus"), NOTE));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// En tapant : plusieurs commandes qui commencent ainsi, une ligne chacune ; une seule, ou
    /// son nom entier : ses formes, et ce qui est enregistré.
    #[test]
    fn typing_a_command_shows_its_forms_and_what_is_saved() {
        let none = |_: &str| Vec::new();
        let l = lines("/t", 20, false, none);
        assert!(l.iter().any(|(t, _)| t.starts_with("/tp")));
        assert!(l.iter().any(|(t, _)| t.starts_with("/timeset")));
        let l = lines("/fps", 20, false, |name: &str| {
            assert_eq!(name, "fps");
            vec!["joueurs : 10".to_string()]
        });
        assert_eq!(l[0].1, TITLE);
        assert!(l.iter().any(|(t, _)| t.contains("/fps set spawn ct")));
        assert!(l.iter().any(|(t, _)| t == "  joueurs : 10"));
        // Avec des arguments : toujours l'aide de la commande.
        let l = lines("/fps set ", 20, false, none);
        assert_eq!(l[0].1, TITLE);
        // Un alias : la même aide.
        assert!(lines("/meteo", 20, false, none)[0]
            .0
            .starts_with("/weather"));
        assert!(lines("/zzz", 20, false, none)[0]
            .0
            .contains("Aucune commande"));
        assert!(lines("//set", 20, false, none).is_empty());
        assert!(lines("bonjour", 20, false, none).is_empty());
        // (v340) `/fps certif` : montré au seul compte qui certifie les cartes.
        assert!(!lines("/fps", 40, false, none)
            .iter()
            .any(|(t, _)| t.contains("certif")));
        assert!(lines("/fps", 40, true, none)
            .iter()
            .any(|(t, _)| t.contains("/fps certif true|false")));
        assert!(!lines("/bot", 40, true, none)
            .iter()
            .any(|(t, _)| t.contains("certif")));
        // Trop de lignes : coupé, avec ce qu'il en reste.
        let l = lines("/", 5, false, none);
        assert_eq!(l.len(), 5);
        assert!(l[4].0.contains("de plus"));
    }
}
