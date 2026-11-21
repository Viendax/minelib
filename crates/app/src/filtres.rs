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

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const SOURCE: &str = "https://dsi.ut-capitole.fr/blacklists/download/child.tar.gz";
const FICHIER: &str = "enfants_ut1.txt";
const CHAQUE: Duration = Duration::from_secs(24 * 3600);
/// Bornes raisonnables : la liste fait moins d'une centaine de domaines.
const MAX_DOMAINES: usize = 5000;
const MAX_ARCHIVE: u64 = 4 * 1024 * 1024;

/// Rafraîchit la liste en arrière-plan si elle a plus d'un jour (ou n'existe pas).
pub fn rafraichir(dossier: PathBuf) {
    let chemin = dossier.join(FICHIER);
    let recente = std::fs::metadata(&chemin)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age < CHAQUE);
    if recente {
        return;
    }
    std::thread::spawn(move || match telecharge(&dossier) {
        Ok(n) => log::info!("filtres : liste enfants de l'UT1 rafraîchie ({n} domaines)"),
        Err(e) => log::warn!("filtres : liste enfants de l'UT1 pas rafraîchie : {e}"),
    });
}

fn telecharge(dossier: &Path) -> Result<usize, String> {
    use std::io::Read;
    let mut resp = crate::update::http()
        .get(SOURCE)
        .header("User-Agent", "minelib-filtres")
        .call()
        .map_err(|e| e.to_string())?;
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
    let texte = dans_tar(&tar, "child/domains").ok_or("pas de child/domains dans l'archive")?;
    let domaines = valide(&texte).ok_or("ce n'est pas une liste de domaines")?;
    std::fs::create_dir_all(dossier).map_err(|e| e.to_string())?;
    let mut sortie = String::from(
        "# Liste « child » de l'Université Toulouse 1 Capitole (Fabrice Prigent), CC BY-SA 4.0.\n\
         # https://dsi.ut-capitole.fr/blacklists/ : sites autorisés aux moins de 10 ans.\n\
         # Rafraîchie chaque jour par minelib ; ne pas modifier (liste_blanche.txt pour les ajouts).\n",
    );
    for d in &domaines {
        sortie.push_str(d);
        sortie.push('\n');
    }
    let tmp = dossier.join(format!("{FICHIER}.tmp"));
    std::fs::write(&tmp, sortie).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dossier.join(FICHIER)).map_err(|e| e.to_string())?;
    Ok(domaines.len())
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

/// Les domaines d'une liste, si c'en est bien une (un nom de domaine par ligne).
fn valide(texte: &str) -> Option<Vec<String>> {
    let mut domaines = Vec::new();
    for ligne in texte.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let ok = ligne.contains('.')
            && ligne.len() <= 253
            && ligne
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
        if !ok {
            return None;
        }
        domaines.push(ligne.to_ascii_lowercase());
    }
    (!domaines.is_empty() && domaines.len() <= MAX_DOMAINES).then_some(domaines)
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
        assert_eq!(valide(&texte).unwrap(), vec!["lumni.fr", "pbskids.org"]);
        assert!(dans_tar(&tar, "child/usage").is_none());
    }

    #[test]
    fn une_page_d_erreur_n_est_pas_une_liste() {
        assert!(valide("<html><body>Erreur 500</body></html>").is_none());
        assert!(valide("").is_none());
        assert!(valide("lumni.fr\n0.0.0.0 mechant.example\n").is_none());
    }
}
