//! (v247) Le navigateur des écrans pour tous les joueurs.
//!
//! Jusqu'à la v246, le navigateur (`minelib-browser.exe` et Chromium, 345 Mo dans
//! `.minelib\cef`) n'était installé que sur le PC du propriétaire, à la main : chez un ami,
//! les écrans montraient leur mire. Maintenant la publication GitHub le compile et le met
//! en ligne (`crates/browser/ci/construire-windows.sh`), et le jeu l'installe tout seul :
//!
//! - **Chromium (CEF)**, la première fois qu'un écran s'affiche : l'archive
//!   `minelib-cef-<CEF>.tar.gz` de la Release `cef-<CEF>` (environ 160 Mo), vérifiée par
//!   son empreinte SHA-256, ouverte dans `.minelib\cef`. Rien d'autre que les fichiers
//!   attendus (noms simples, dossier `locales` seulement).
//! - **Le navigateur**, à la version du jeu : `minelib-browser_v<N>.exe` de la Release
//!   `v<N>`, vérifié de même. Une version du jeu sans navigateur publié (version de
//!   développement) garde celui qui est là.
//!
//! On ne remplace que ce que ce module a installé lui-même (fichiers `cef-version.txt` et
//! `navigateur-version.txt`) ou ce qui manque : un navigateur posé à la main reste tel
//! quel. Le travail se fait dans un fil à part, une fois par lancement du jeu ; pendant
//! ce temps, les écrans attendent (mire) et le joueur est prévenu dans le tchat.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};

/// Version de CEF du navigateur (celle de `cef-dll-sys` dans `crates/browser`).
pub const CEF_VERSION: &str = "150.0.14";
const BASE: &str = "https://github.com/Viendax/minelib/releases/download";
/// Bornes : l'archive fait environ 160 Mo, ouverte environ 345 Mo.
const MAX_ARCHIVE: u64 = 400 * 1024 * 1024;
const MAX_OUVERT: u64 = 900 * 1024 * 1024;
const MAX_EXE: u64 = 64 * 1024 * 1024;
const NOTE_CEF: &str = "cef-version.txt";
const NOTE_NAV: &str = "navigateur-version.txt";
const EXE: &str = "minelib-browser.exe";

/// Où en est l'installation.
#[derive(Clone, Debug, PartialEq)]
pub enum Etat {
    /// Rien n'a été lancé (ou rien à faire).
    Rien,
    /// En cours ; les écrans attendent.
    EnCours,
    /// Fini (installé, ou rien à faire) : le jeu peut lancer le navigateur.
    Fini,
    /// Raté : les écrans gardent leur mire jusqu'au prochain lancement du jeu.
    Rate,
}

struct Partage {
    etat: Etat,
    /// Messages pour le tchat, pas encore dits.
    mots: Vec<String>,
}

static PARTAGE: Mutex<Partage> = Mutex::new(Partage {
    etat: Etat::Rien,
    mots: Vec::new(),
});

fn partage() -> std::sync::MutexGuard<'static, Partage> {
    PARTAGE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Les messages pour le joueur depuis le dernier appel.
pub fn mots() -> Vec<String> {
    std::mem::take(&mut partage().mots)
}

fn dire(mot: String) {
    log::info!("navigateur des écrans : {mot}");
    partage().mots.push(mot);
}

/// Ce qu'il faut installer dans `dossier` pour la version `version` du jeu.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Besoin {
    pub cef: bool,
    pub exe: bool,
}

impl Besoin {
    pub fn rien(self) -> bool {
        !self.cef && !self.exe
    }
}

pub fn besoin(dossier: &Path, version: u32) -> Besoin {
    let note = |nom: &str| std::fs::read_to_string(dossier.join(nom)).ok();
    let cef = !dossier.join("libcef.dll").is_file()
        || note(NOTE_CEF).is_some_and(|v| v.trim() != CEF_VERSION);
    let exe = !dossier.join(EXE).is_file()
        || note(NOTE_NAV).is_some_and(|v| v.trim() != version.to_string());
    Besoin { cef, exe }
}

/// Lance l'installation dans `dossier` si quelque chose manque (une fois par lancement du
/// jeu). Rend l'état : `EnCours` tant qu'il faut attendre.
pub fn lancer(dossier: PathBuf, version: u32) -> Etat {
    if !cfg!(windows) {
        return Etat::Fini;
    }
    {
        let mut p = partage();
        if p.etat != Etat::Rien {
            return p.etat.clone();
        }
        let b = besoin(&dossier, version);
        if b.rien() {
            p.etat = Etat::Fini;
            return Etat::Fini;
        }
        p.etat = Etat::EnCours;
    }
    std::thread::spawn(move || {
        let b = besoin(&dossier, version);
        let fin = installer(&dossier, version, b);
        let etat = match fin {
            Ok(true) => {
                dire("navigateur des écrans installé.".into());
                Etat::Fini
            }
            Ok(false) => Etat::Fini,
            Err(e) => {
                let ok_avant = dossier.join(EXE).is_file() && dossier.join("libcef.dll").is_file();
                dire(format!(
                    "navigateur des écrans pas installé ({e:#}) ; nouvel essai au prochain lancement."
                ));
                if ok_avant {
                    Etat::Fini
                } else {
                    Etat::Rate
                }
            }
        };
        partage().etat = etat;
    });
    Etat::EnCours
}

/// `Ok(true)` : quelque chose a été installé ; `Ok(false)` : rien de publié pour cette
/// version (navigateur gardé tel quel).
fn installer(dossier: &Path, version: u32, b: Besoin) -> Result<bool> {
    std::fs::create_dir_all(dossier).context("dossier cef")?;
    let mut fait = false;
    if b.cef {
        dire(format!(
            "téléchargement de Chromium pour les écrans (environ 160 Mo, une seule fois)…"
        ));
        let nom = format!("minelib-cef-{CEF_VERSION}.tar.gz");
        let url = format!("{BASE}/cef-{CEF_VERSION}/{nom}");
        let tmp = dossier.join(format!("{nom}.tmp"));
        let attendu = empreinte(&format!("{url}.sha256"))?.ok_or(anyhow!("Chromium pas publié"))?;
        telecharge(&url, &tmp, MAX_ARCHIVE, attendu)?;
        let ouvert = std::fs::File::open(&tmp)
            .context("archive")
            .and_then(|f| ouvre(flate2::read::GzDecoder::new(std::io::BufReader::new(f)), dossier));
        let _ = std::fs::remove_file(&tmp);
        let n = ouvert?;
        std::fs::write(dossier.join(NOTE_CEF), CEF_VERSION).context("note cef")?;
        log::info!("navigateur des écrans : Chromium {CEF_VERSION} installé ({n} fichiers)");
        fait = true;
    }
    if b.exe {
        let nom = format!("minelib-browser_v{version}.exe");
        let url = format!("{BASE}/v{version}/{nom}");
        match empreinte(&format!("{url}.sha256"))? {
            None => {
                log::info!("navigateur des écrans : pas de navigateur publié pour la v{version}");
                if !dossier.join(EXE).is_file() {
                    return Err(anyhow!("pas de navigateur publié pour la v{version}"));
                }
            }
            Some(attendu) => {
                let neuf = dossier.join("minelib-browser.new.exe");
                telecharge(&url, &neuf, MAX_EXE, attendu)?;
                remplace(dossier, &neuf)?;
                std::fs::write(dossier.join(NOTE_NAV), version.to_string()).context("note")?;
                log::info!("navigateur des écrans : navigateur v{version} installé");
                fait = true;
            }
        }
    }
    Ok(fait)
}

/// L'empreinte publiée à `url` (`None` : pas publiée, 404).
fn empreinte(url: &str) -> Result<Option<[u8; 32]>> {
    match crate::update::http()
        .get(url)
        .header("User-Agent", "minelib-navigateur")
        .call()
    {
        Ok(mut r) => {
            let texte = r.body_mut().read_to_string().context("empreinte")?;
            crate::update::parse_sha256(&texte)
                .map(Some)
                .ok_or(anyhow!("empreinte illisible"))
        }
        Err(ureq::Error::StatusCode(404)) => Ok(None),
        Err(e) => Err(anyhow!(e)).context("GitHub injoignable"),
    }
}

/// Télécharge `url` dans `vers`, au plus `max` octets, et vérifie son empreinte.
fn telecharge(url: &str, vers: &Path, max: u64, attendu: [u8; 32]) -> Result<()> {
    use std::io::Write;
    let mut r = crate::update::http()
        .get(url)
        .header("User-Agent", "minelib-navigateur")
        .call()
        .context("téléchargement")?;
    let mut lu = r.body_mut().with_config().limit(max).reader();
    let mut f = std::fs::File::create(vers).context("écriture")?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = lu.read(&mut buf).context("téléchargement interrompu")?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        f.write_all(&buf[..n]).context("écriture")?;
    }
    f.flush().context("écriture")?;
    drop(f);
    let recu: [u8; 32] = h.finalize().into();
    if recu != attendu {
        let _ = std::fs::remove_file(vers);
        return Err(anyhow!("empreinte différente de celle publiée : fichier refusé"));
    }
    Ok(())
}

/// Met le navigateur neuf à la place de l'ancien (qui peut tourner encore : Windows laisse
/// renommer un programme ouvert).
fn remplace(dossier: &Path, neuf: &Path) -> Result<()> {
    let exe = dossier.join(EXE);
    if exe.is_file() {
        let mut vieux = dossier.join("minelib-browser.old.exe");
        if vieux.exists() && std::fs::remove_file(&vieux).is_err() {
            vieux = dossier.join(format!("minelib-browser.old{}.exe", std::process::id()));
        }
        std::fs::rename(&exe, &vieux).context("mise de côté de l'ancien navigateur")?;
    }
    std::fs::rename(neuf, &exe).context("mise en place du navigateur")?;
    Ok(())
}

/// Un nom de l'archive qu'on accepte : `fichier.ext` ou `locales/fichier.ext`, en lettres,
/// chiffres, `.`, `-` et `_` — rien qui sorte du dossier.
fn nom_sur(nom: &str) -> Option<PathBuf> {
    let nom = nom.trim_start_matches("./");
    let morceaux: Vec<&str> = nom.split('/').collect();
    let ok = |m: &str| {
        !m.is_empty()
            && !m.starts_with('.')
            && m.len() <= 100
            && m.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_')
    };
    match morceaux.as_slice() {
        [f] if ok(f) => Some(PathBuf::from(f)),
        ["locales", f] if ok(f) => Some(Path::new("locales").join(f)),
        _ => None,
    }
}

/// Ouvre une archive tar (ustar) dans `dossier`, au fil de la lecture : rend le nombre de
/// fichiers écrits. Un nom refusé, un type d'entrée autre que fichier ou dossier, ou plus
/// de `MAX_OUVERT` octets font tout refuser.
fn ouvre<R: Read>(mut r: R, dossier: &Path) -> Result<usize> {
    let mut tete = [0u8; 512];
    let mut total: u64 = 0;
    let mut n = 0;
    loop {
        r.read_exact(&mut tete).context("archive tronquée")?;
        if tete.iter().all(|&b| b == 0) {
            return Ok(n);
        }
        let champ = |a: usize, b: usize| {
            let s = &tete[a..b];
            let fin = s.iter().position(|&c| c == 0).unwrap_or(s.len());
            String::from_utf8_lossy(&s[..fin]).trim().to_string()
        };
        let mut nom = champ(0, 100);
        let prefixe = champ(345, 500);
        if !prefixe.is_empty() {
            nom = format!("{prefixe}/{nom}");
        }
        let taille = u64::from_str_radix(&champ(124, 136), 8).map_err(|_| anyhow!("archive abîmée"))?;
        total += taille;
        if total > MAX_OUVERT {
            return Err(anyhow!("archive trop grande"));
        }
        let bourrage = (512 - taille % 512) % 512;
        match tete[156] {
            b'5' => {
                // Dossier : seulement `locales` (ou le dossier de l'archive lui-même).
                match nom.trim_start_matches("./").trim_end_matches('/') {
                    "" => {}
                    "locales" => {
                        std::fs::create_dir_all(dossier.join("locales")).context("dossier locales")?
                    }
                    _ => return Err(anyhow!("dossier inattendu dans l'archive : {nom}")),
                }
                std::io::copy(&mut (&mut r).take(taille + bourrage), &mut std::io::sink())?;
            }
            b'0' | 0 => {
                let rel = nom_sur(&nom).ok_or_else(|| anyhow!("nom refusé dans l'archive : {nom}"))?;
                let chemin = dossier.join(&rel);
                if let Some(p) = chemin.parent() {
                    std::fs::create_dir_all(p).context("dossier")?;
                }
                let tmp = chemin.with_extension("part");
                let mut f = std::fs::File::create(&tmp).with_context(|| format!("écriture de {nom}"))?;
                let copie = std::io::copy(&mut (&mut r).take(taille), &mut f)?;
                if copie != taille {
                    return Err(anyhow!("archive tronquée"));
                }
                drop(f);
                std::fs::rename(&tmp, &chemin).with_context(|| format!("mise en place de {nom}"))?;
                std::io::copy(&mut (&mut r).take(bourrage), &mut std::io::sink())?;
                n += 1;
            }
            t => return Err(anyhow!("entrée refusée dans l'archive ({nom}, type {})", t as char)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entree(nom: &str, type_: u8, contenu: &[u8]) -> Vec<u8> {
        let mut tete = [0u8; 512];
        tete[..nom.len()].copy_from_slice(nom.as_bytes());
        let taille = format!("{:011o}\0", contenu.len());
        tete[124..136].copy_from_slice(taille.as_bytes());
        tete[156] = type_;
        let mut v = tete.to_vec();
        v.extend_from_slice(contenu);
        v.resize(512 + contenu.len().div_ceil(512) * 512, 0);
        v
    }

    fn dossier(nom: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("minelib-nav-{nom}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn l_archive_de_chromium_s_ouvre_dans_le_dossier() {
        let mut tar = entree("libcef.dll", b'0', b"MZ dll");
        tar.extend(entree("locales/", b'5', b""));
        tar.extend(entree("locales/fr.pak", b'0', &[7u8; 700]));
        tar.extend([0u8; 1024]);
        let d = dossier("ok");
        assert_eq!(ouvre(&tar[..], &d).unwrap(), 2);
        assert_eq!(std::fs::read(d.join("libcef.dll")).unwrap(), b"MZ dll");
        assert_eq!(std::fs::read(d.join("locales").join("fr.pak")).unwrap().len(), 700);
        std::fs::remove_dir_all(&d).ok();
    }

    /// L'archive vraiment publiée (faite par `crates/browser/ci/construire-windows.sh`) :
    /// `MINELIB_CEF_TGZ=<chemin> cargo test -p minelib -- --ignored archive_publiee`.
    #[test]
    #[ignore]
    fn l_archive_publiee_s_ouvre() {
        let chemin = std::env::var("MINELIB_CEF_TGZ").expect("MINELIB_CEF_TGZ");
        let f = std::fs::File::open(chemin).unwrap();
        let d = dossier("publiee");
        let n = ouvre(flate2::read::GzDecoder::new(std::io::BufReader::new(f)), &d).unwrap();
        assert_eq!(n, 17);
        assert!(d.join("libcef.dll").metadata().unwrap().len() > 200_000_000);
        assert!(d.join("locales").join("fr.pak").is_file());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn rien_ne_sort_du_dossier() {
        for mauvais in [
            "../minelib.exe",
            "/etc/passwd",
            "locales/../../x.dll",
            "a/b/c.dll",
            "C:\\Windows\\x.dll",
            ".cache",
        ] {
            assert!(nom_sur(mauvais).is_none(), "{mauvais}");
            let mut tar = entree(mauvais, b'0', b"x");
            tar.extend([0u8; 1024]);
            let d = dossier("mauvais");
            assert!(ouvre(&tar[..], &d).is_err(), "{mauvais}");
            std::fs::remove_dir_all(&d).ok();
        }
        // Lien symbolique : refusé.
        let mut tar = entree("libcef.dll", b'2', b"");
        tar.extend([0u8; 1024]);
        let d = dossier("lien");
        assert!(ouvre(&tar[..], &d).is_err());
        std::fs::remove_dir_all(&d).ok();
        assert_eq!(nom_sur("./libcef.dll"), Some(PathBuf::from("libcef.dll")));
    }

    /// La publication et le jeu parlent du même Chromium.
    #[test]
    fn la_publication_fait_le_chromium_que_le_jeu_attend() {
        let script = include_str!("../../browser/ci/construire-windows.sh");
        assert!(script.contains(&format!("CEF_VERSION=\"{CEF_VERSION}\"")));
        let cargo = include_str!("../../browser/Cargo.lock");
        assert!(cargo.contains(&format!("+{CEF_VERSION}\"")), "cef-dll-sys de crates/browser");
    }

    #[test]
    fn on_n_installe_que_ce_qui_manque_ou_ce_qu_on_a_pose() {
        let d = dossier("besoin");
        assert_eq!(besoin(&d, 247), Besoin { cef: true, exe: true });
        std::fs::write(d.join("libcef.dll"), b"x").unwrap();
        std::fs::write(d.join(EXE), b"x").unwrap();
        // Posé à la main (sans note) : gardé.
        assert!(besoin(&d, 247).rien());
        // Posé par le jeu pour une autre version : remplacé.
        std::fs::write(d.join(NOTE_NAV), "246").unwrap();
        std::fs::write(d.join(NOTE_CEF), CEF_VERSION).unwrap();
        assert_eq!(besoin(&d, 247), Besoin { cef: false, exe: true });
        std::fs::write(d.join(NOTE_CEF), "149.0.1").unwrap();
        assert_eq!(besoin(&d, 246), Besoin { cef: true, exe: false });
        std::fs::remove_dir_all(&d).ok();
    }
}
