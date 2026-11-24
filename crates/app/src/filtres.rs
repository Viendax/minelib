//! (v208) La liste des sites pour enfants de l'Université Toulouse 1 Capitole, rafraîchie
//! une fois par jour dans `.minelib\filtres\enfants_ut1.txt`.
//!
//! C'est la catégorie « child » des listes de l'UT1 (Fabrice Prigent, licence CC BY-SA
//! 4.0) : « sites autorisés aux enfants de moins de 10 ans », mise à jour par l'université.
//! Le navigateur l'ajoute à sa liste blanche (en plus de la copie qu'il a en lui), ce qui
//! en fait les sites qu'on peut ouvrir sur les écrans en ligne.
//!
//! Téléchargée en HTTPS depuis le site de l'université, jamais d'un miroir : une liste
//! blanche ajoute des sites, il ne faut la prendre qu'à sa source. Un fichier qui ne
//! ressemble pas à une liste de domaines est ignoré, et l'ancien reste.
//!
//! (v243) Le jeu est interdit aux moins de 18 ans et les écrans ouvrent le web sauf les
//! interdits : on rafraîchit aussi, dans `noire_ut1.txt`, la liste noire de l'université
//! (contenus adultes mêlés, jeux d'argent sauf les opérateurs agréés en France, drogue,
//! piratage, minage caché, attaques, logiciels espions, armes, haine, contournement du
//! filtrage, hameçonnage). Le navigateur l'ajoute à sa copie intégrée. Le porno lui-même
//! (4,6 millions de domaines, 18 Mo par jour) est laissé au DNS familial de Cloudflare.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const SOURCES: &str = "https://dsi.ut-capitole.fr/blacklists/download/";
const CHAQUE: Duration = Duration::from_secs(24 * 3600);
/// La plus grosse archive, l'hameçonnage, fait 2,4 Mo (5 Mo une fois ouverte).
const MAX_ARCHIVE: u64 = 16 * 1024 * 1024;

/// Une liste rafraîchie chaque jour : son fichier, les catégories de l'UT1 qui la font.
struct Liste {
    nom: &'static str,
    fichier: &'static str,
    categories: &'static [&'static str],
    /// Catégories dont les domaines sont retirés de la liste.
    sauf: &'static [&'static str],
    /// Borne raisonnable par catégorie (la plus grosse, la presse, en a moins de 5000).
    max: usize,
    entete: &'static str,
}

const LISTES: [Liste; 2] = [
    Liste {
        nom: "liste enfants",
        fichier: "enfants_ut1.txt",
        categories: &["child"],
        sauf: &[],
        max: 5000,
        entete: "# Liste « child » de l'Université Toulouse 1 Capitole (Fabrice Prigent), CC BY-SA 4.0.\n\
                 # https://dsi.ut-capitole.fr/blacklists/ : sites autorisés aux moins de 10 ans.\n\
                 # Rafraîchie chaque jour par minelib ; ne pas modifier (liste_blanche.txt pour les ajouts).\n",
    },
    Liste {
        nom: "liste noire",
        fichier: "noire_ut1.txt",
        categories: &[
            "mixed_adult",
            "gambling",
            "drogue",
            "warez",
            "cryptojacking",
            "ddos",
            "stalkerware",
            "dangerous_material",
            "agressif",
            "redirector",
            "phishing",
        ],
        sauf: &["arjel"],
        max: 2_000_000,
        entete: "# Liste noire de l'Université Toulouse 1 Capitole (Fabrice Prigent), CC BY-SA 4.0,\n\
                 # https://dsi.ut-capitole.fr/blacklists/ : mixed_adult, gambling (moins arjel),\n\
                 # drogue, warez, cryptojacking, ddos, stalkerware, dangerous_material, agressif,\n\
                 # redirector, phishing. Rafraîchie chaque jour par minelib ; ne pas modifier\n\
                 # (liste_noire.txt pour les ajouts).\n",
    },
];

/// Rafraîchit les listes en arrière-plan si elles ont plus d'un jour (ou n'existent pas).
pub fn rafraichir(dossier: PathBuf) {
    for liste in &LISTES {
        let chemin = dossier.join(liste.fichier);
        let recente = std::fs::metadata(&chemin)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age < CHAQUE);
        if recente {
            continue;
        }
        let dossier = dossier.clone();
        std::thread::spawn(move || match telecharge(&dossier, liste) {
            Ok(n) => log::info!("filtres : {} de l'UT1 rafraîchie ({n} domaines)", liste.nom),
            Err(e) => log::warn!("filtres : {} de l'UT1 pas rafraîchie : {e}", liste.nom),
        });
    }
}

/// Toutes les catégories de la liste, ou rien : on garde l'ancienne plutôt qu'une liste
/// à laquelle manquerait une catégorie.
fn telecharge(dossier: &Path, liste: &Liste) -> Result<usize, String> {
    let lit = |c: &str| -> Result<Vec<String>, String> {
        valide(&categorie(c)?, liste.max).ok_or(format!("{c} : ce n'est pas une liste de domaines"))
    };
    let mut domaines = Vec::new();
    for c in liste.categories {
        domaines.append(&mut lit(c)?);
    }
    let mut sauf = Vec::new();
    for c in liste.sauf {
        sauf.append(&mut lit(c)?);
    }
    domaines.sort();
    domaines.dedup();
    sauf.sort();
    domaines.retain(|d| sauf.binary_search(d).is_err());
    std::fs::create_dir_all(dossier).map_err(|e| e.to_string())?;
    let mut sortie = String::from(liste.entete);
    for d in &domaines {
        sortie.push_str(d);
        sortie.push('\n');
    }
    let tmp = dossier.join(format!("{}.tmp", liste.fichier));
    std::fs::write(&tmp, sortie).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dossier.join(liste.fichier)).map_err(|e| e.to_string())?;
    Ok(domaines.len())
}

/// Les domaines d'une catégorie de l'UT1, depuis son archive sur le site de l'université.
fn categorie(c: &str) -> Result<String, String> {
    use std::io::Read;
    let mut resp = crate::update::http()
        .get(&format!("{SOURCES}{c}.tar.gz"))
        .header("User-Agent", "minelib-filtres")
        .call()
        .map_err(|e| format!("{c} : {e}"))?;
    let mut gz = Vec::new();
    resp.body_mut()
        .as_reader()
        .take(MAX_ARCHIVE)
        .read_to_end(&mut gz)
        .map_err(|e| e.to_string())?;
    let mut tar = Vec::new();
    flate2::read::GzDecoder::new(&gz[..])
        .take(MAX_ARCHIVE * 4)
        .read_to_end(&mut tar)
        .map_err(|e| e.to_string())?;
    dans_tar(&tar, &format!("{c}/domains")).ok_or(format!("pas de {c}/domains dans l'archive"))
}

/// Le contenu d'un fichier d'une archive tar (format ustar, sans extension).
fn dans_tar(tar: &[u8], nom: &str) -> Option<String> {
    let mut i = 0;
    while i + 512 <= tar.len() {
        let tete = &tar[i..i + 512];
        if tete.iter().all(|&b| b == 0) {
            return None;
        }
        let champ = |a: usize, b: usize| {
            let s = &tete[a..b];
            let fin = s.iter().position(|&c| c == 0).unwrap_or(s.len());
            String::from_utf8_lossy(&s[..fin]).trim().to_string()
        };
        let mut chemin = champ(0, 100);
        let prefixe = champ(345, 500);
        if !prefixe.is_empty() {
            chemin = format!("{prefixe}/{chemin}");
        }
        let taille = usize::from_str_radix(&champ(124, 136), 8).ok()?;
        let debut = i + 512;
        let fin = debut.checked_add(taille)?;
        if fin > tar.len() {
            return None;
        }
        if chemin.trim_start_matches("./") == nom {
            return Some(String::from_utf8_lossy(&tar[debut..fin]).into_owned());
        }
        i = debut + taille.div_ceil(512) * 512;
    }
    None
}

/// Les domaines d'une liste, si c'en est bien une (un nom de domaine par ligne ; le `_`,
/// interdit dans un nom d'hôte, se trouve pourtant dans quelques domaines des listes).
fn valide(texte: &str, max: usize) -> Option<Vec<String>> {
    let mut domaines = Vec::new();
    for ligne in texte.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let ok = ligne.contains('.')
            && ligne.len() <= 253
            && ligne
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_');
        if !ok {
            return None;
        }
        domaines.push(ligne.to_ascii_lowercase());
    }
    (!domaines.is_empty() && domaines.len() <= max).then_some(domaines)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entree(nom: &str, contenu: &[u8]) -> Vec<u8> {
        let mut tete = [0u8; 512];
        tete[..nom.len()].copy_from_slice(nom.as_bytes());
        let taille = format!("{:011o}\0", contenu.len());
        tete[124..136].copy_from_slice(taille.as_bytes());
        let mut v = tete.to_vec();
        v.extend_from_slice(contenu);
        v.resize(512 + contenu.len().div_ceil(512) * 512, 0);
        v
    }

    #[test]
    fn on_lit_la_liste_dans_l_archive() {
        let mut tar = entree("child/urls", b"exemple.fr/page/\n");
        tar.extend(entree("child/domains", b"lumni.fr\npbskids.org\n"));
        tar.extend([0u8; 1024]);
        let texte = dans_tar(&tar, "child/domains").unwrap();
        assert_eq!(valide(&texte, 5000).unwrap(), vec!["lumni.fr", "pbskids.org"]);
        assert!(dans_tar(&tar, "child/usage").is_none());
    }

    #[test]
    fn une_page_d_erreur_n_est_pas_une_liste() {
        assert!(valide("<html><body>Erreur 500</body></html>", 5000).is_none());
        assert!(valide("", 5000).is_none());
        assert!(valide("lumni.fr\n0.0.0.0 mechant.example\n", 5000).is_none());
        assert!(valide("a.fr\nb.fr\nc.fr\n", 2).is_none());
    }
}
