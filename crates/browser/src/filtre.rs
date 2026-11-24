//! Ce qui peut s'ouvrir sur un écran en jeu, et rien d'autre. (v205)
//!
//! (v243) **Le jeu est interdit aux moins de 18 ans**, et le propriétaire a choisi « le web
//! ouvert sauf les interdits » : tout s'ouvre, sauf le porno, les jeux d'argent non agréés,
//! la drogue, le piratage, les arnaques et les virus, et les sites qui servent à contourner
//! ce filtre. La liste noire devient le mode par défaut, en ligne comme en solo :
//!
//! - **DNS chiffré familial de Cloudflare** (`family.cloudflare-dns.com`, voir `main.rs`) :
//!   les sites pour adultes et les logiciels malveillants ne se résolvent pas ;
//! - **listes de l'Université Toulouse 1 Capitole** : une copie intégrée
//!   (`ut1_noire.txt.gz` : contenus adultes mêlés, jeux d'argent sauf les opérateurs
//!   agréés en France, drogue, piratage, minage caché, attaques, logiciels espions, armes,
//!   haine, contournement du filtrage), plus la version du jour que le jeu télécharge
//!   (`noire_ut1.txt`, avec en plus l'hameçonnage), plus `liste_noire.txt` à la main ;
//! - **Twitch et Vimeo** sont refusés avec leur raison : leurs vidéos sont en H.264 et AAC,
//!   que ce navigateur (CEF sans codecs propriétaires) ne lit pas (essai du 24/11/2026).
//!
//! Chaque joueur ne voit que les pages qu'il ouvre lui-même (« regarder ensemble » ne
//! partage qu'un numéro de vidéo YouTube).
//!
//! Avant (v205-v242), le jeu était tout public. Trois barrières, dans cet ordre :
//!
//! 1. **Les schémas et les adresses privées** sont refusés quoi qu'il arrive : rien que
//!    `http` et `https` vers un nom de domaine. Donc pas de `file://` qui lirait un
//!    fichier du disque, pas de `chrome://`, pas de `javascript:`, pas de `data:`, pas de
//!    `localhost`, pas de `192.168.x.x` ni d'adresse écrite en chiffres.
//! 2. **Mode liste blanche** (le défaut en ligne) : seuls les domaines de la liste
//!    s'ouvrent. C'est la seule barrière vraiment étanche, parce qu'elle ne dépend pas
//!    d'une liste de méchants tenue à jour.
//! 3. **Mode liste noire** (solo) : tout s'ouvre sauf les domaines de la liste. La liste
//!    vient de l'Université Toulouse 1 Capitole (voir `claude/ecran-en-jeu.md`), mise à
//!    jour tous les jours. Sans fichier de liste, on retombe sur la liste blanche : jamais
//!    « tout ouvert par défaut ».
//!
//! (v208) La liste blanche intégrée : YouTube et Google (ci-dessous), la liste « child »
//! de l'Université Toulouse 1 Capitole (sites pour les moins de 10 ans, tenue à jour par
//! elle ; copie dans `ut1_enfants.txt`, version fraîche téléchargée chaque jour par le jeu
//! dans `enfants_ut1.txt`), et quelques sites choisis à la main (`CHOISIS`). Pas de
//! Wikipédia (« Wikipedia is not censored » : des images explicites y restent), ni de
//! plateforme de vidéos d'inconnus autre que YouTube en mode restreint.
//!
//! En ligne (`sans_compte`), aucune page de connexion Google ne s'ouvre : on regarde
//! ensemble chacun avec ses pubs, personne avec l'abonnement d'un autre.
//!
//! Les publicités de YouTube ne sont **pas** bloquées, et leurs domaines sont dans la
//! liste blanche : les conditions de YouTube interdisent d'empêcher une publicité de
//! passer, et c'est ce qui rend le « regarder ensemble » légitime.

use std::collections::HashSet;
use std::path::Path;

/// Comment on filtre.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Seuls les domaines autorisés s'ouvrent.
    Blanche,
    /// Tout s'ouvre sauf les domaines refusés.
    Noire,
}

impl Mode {
    pub fn from_name(name: &str) -> Option<Mode> {
        match name.trim().to_ascii_lowercase().as_str() {
            "blanche" | "white" | "allowlist" => Some(Mode::Blanche),
            "noire" | "black" | "blocklist" => Some(Mode::Noire),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Mode::Blanche => "blanche",
            Mode::Noire => "noire",
        }
    }
}

/// Pourquoi une page a été refusée. Le numéro part au jeu, qui l'écrit au joueur.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refus {
    /// Pas `http` ou `https` (fichier du disque, page interne, script…).
    Schema = 1,
    /// Une machine du réseau local, ou une adresse écrite en chiffres.
    Privee = 2,
    /// Hors de la liste blanche.
    HorsBlanche = 3,
    /// Dans la liste noire.
    Noire = 4,
    /// (v208) Une page de connexion à un compte, en ligne.
    Compte = 5,
    /// (v243) Twitch ou Vimeo : leurs vidéos ne se lisent pas dans ce navigateur.
    Lecture = 6,
}

impl Refus {
    pub fn raison(self) -> &'static str {
        match self {
            Refus::Schema => "ce genre d'adresse ne s'ouvre pas sur un écran",
            Refus::Privee => "pas de machine du réseau local sur un écran",
            Refus::HorsBlanche => "site hors de la liste autorisée",
            Refus::Noire => "site filtré",
            Refus::Compte => "pas de connexion à un compte sur les écrans en ligne",
            Refus::Lecture => {
                "Twitch et Vimeo ne se lisent pas sur les écrans (leurs vidéos demandent des \
                 codecs que le navigateur n'a pas)"
            }
        }
    }
}

/// Domaines autorisés d'office en mode liste blanche : de quoi regarder YouTube et
/// chercher sur Google, publicités comprises (voir le mot d'en-tête du module).
const BASE_BLANCHE: &[&str] = &[
    // YouTube et ses serveurs d'images, de vidéo et de miniatures.
    "youtube.com",
    "youtu.be",
    "youtube-nocookie.com",
    "ytimg.com",
    "ggpht.com",
    "googlevideo.com",
    "youtubei.googleapis.com",
    // Google : la recherche, les comptes, les ressources communes.
    "google.com",
    "gstatic.com",
    "googleapis.com",
    "googleusercontent.com",
    "withgoogle.com",
    // Les publicités de YouTube : ne pas les bloquer est une condition de YouTube.
    "googlesyndication.com",
    "googletagservices.com",
    "doubleclick.net",
    "2mdn.net",
    "googleadservices.com",
];

/// (v208) Sites ajoutés à la main, tout public et libres de consultation :
/// Vikidia, l'encyclopédie des 8-13 ans (CC BY-SA, contenu choquant exclu par ses principes
/// fondateurs), et la NASA (images et vidéos sans droit d'auteur aux États-Unis).
const CHOISIS: &[&str] = &["vikidia.org", "nasa.gov"];

/// (v208) La liste « child » de l'UT1 au moment de la compilation.
const UT1_ENFANTS: &str = include_str!("ut1_enfants.txt");

/// (v243) La liste noire de l'UT1 au moment de la compilation, compressée (voir le mot
/// d'en-tête du module).
const UT1_NOIRE: &[u8] = include_bytes!("ut1_noire.txt.gz");

/// (v243) Les plateformes dont les vidéos ne se lisent pas ici (H.264 et AAC).
const SANS_LECTURE: &[&str] = &["twitch.tv", "vimeo.com"];

/// (v243) Une liste de domaines rangée en empreintes de 64 bits triées : quelques centaines
/// de milliers de domaines en quelques Mo de mémoire, au lieu de dizaines.
#[derive(Debug, Clone, Default)]
pub struct Empreintes(Vec<u64>);

impl Empreintes {
    /// Ajoute les domaines d'un texte (même format que `domaines`) ; rend leur nombre.
    fn ajoute(&mut self, texte: &str) -> usize {
        let avant = self.0.len();
        chaque_domaine(texte, |d| self.0.push(empreinte(d)));
        self.0.len() - avant
    }

    /// À faire après les ajouts, avant de chercher.
    fn range(&mut self) {
        self.0.sort_unstable();
        self.0.dedup();
    }

    fn contient(&self, domaine: &str) -> bool {
        self.0.binary_search(&empreinte(domaine)).is_ok()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
}

/// FNV-1a sur 64 bits : stable d'une version à l'autre, et deux domaines n'ont qu'une
/// chance sur des milliards de milliards d'avoir la même.
fn empreinte(d: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in d.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// La copie intégrée de la liste noire de l'UT1.
fn ut1_noire() -> String {
    use std::io::Read;
    let mut texte = String::new();
    let _ = flate2::read::GzDecoder::new(UT1_NOIRE).read_to_string(&mut texte);
    texte
}

/// Le filtre d'un navigateur : son mode et ses deux listes.
#[derive(Debug, Clone)]
pub struct Filtre {
    mode: Mode,
    blanche: HashSet<String>,
    noire: Empreintes,
    /// (v208) En ligne : pas de page de connexion à un compte.
    sans_compte: bool,
}

impl Filtre {
    /// Le filtre demandé, avec les listes du dossier s'il y en a. Rend aussi ce qu'il faut
    /// dire au journal (pourquoi on n'a pas eu ce qui était demandé).
    pub fn charge(mode: Mode, dossier: Option<&Path>) -> (Filtre, String) {
        let lire_texte = |nom: &str| -> Option<String> {
            std::fs::read_to_string(dossier?.join(nom)).ok()
        };
        let lire = |nom: &str| -> Option<(HashSet<String>, usize)> {
            let set = domaines(&lire_texte(nom)?);
            let n = set.len();
            Some((set, n))
        };
        let mut blanche: HashSet<String> = BASE_BLANCHE
            .iter()
            .chain(CHOISIS)
            .map(|d| d.to_string())
            .collect();
        blanche.extend(domaines(UT1_ENFANTS));
        let mut mot = String::new();
        if let Some((plus, n)) = lire("enfants_ut1.txt") {
            blanche.extend(plus);
            mot.push_str(&format!("liste enfants de l'UT1 du jour : {n} domaines ; "));
        }
        if let Some((plus, n)) = lire("liste_blanche.txt") {
            blanche.extend(plus);
            mot.push_str(&format!("liste blanche : {n} domaines en plus ; "));
        }
        // (v243) La liste noire : la copie intégrée de l'UT1, celle du jour, et celle du
        // propriétaire. Toujours là, donc jamais de repli sur la liste blanche.
        let mut noire = Empreintes::default();
        if mode == Mode::Noire {
            let n = noire.ajoute(&ut1_noire());
            mot.push_str(&format!("liste noire intégrée de l'UT1 : {n} domaines ; "));
            for (nom, quoi) in [("noire_ut1.txt", "de l'UT1 du jour"), ("liste_noire.txt", "à la main")] {
                if let Some(texte) = lire_texte(nom) {
                    let n = noire.ajoute(&texte);
                    mot.push_str(&format!("liste noire {quoi} : {n} domaines ; "));
                }
            }
            noire.range();
            mot.push_str(&format!("liste noire en tout : {} domaines ; ", noire.len()));
        }
        mot.push_str(&format!("mode {}", mode.name()));
        (
            Filtre {
                mode,
                blanche,
                noire,
                sans_compte: false,
            },
            mot,
        )
    }

    /// (v208) En ligne : plus aucune page de connexion à un compte.
    pub fn sans_compte(mut self) -> Self {
        self.sans_compte = true;
        self
    }

    /// Cette adresse peut-elle s'ouvrir ?
    pub fn juge(&self, url: &str) -> Result<(), Refus> {
        let url = url.trim();
        // Une page vide est la page de départ d'un onglet : rien à filtrer.
        if url.is_empty() || url.eq_ignore_ascii_case("about:blank") {
            return Ok(());
        }
        let Some(hote) = hote(url) else {
            return Err(Refus::Schema);
        };
        if hote.is_empty() || !hote.contains('.') || adresse_chiffree(&hote) {
            return Err(Refus::Privee);
        }
        // (v210) La page du lecteur, que le navigateur sert lui-même.
        if est_lecteur(url) {
            return Ok(());
        }
        if self.sans_compte && connexion(&hote) {
            return Err(Refus::Compte);
        }
        if SANS_LECTURE.iter().any(|d| sous(&hote, d)) {
            return Err(Refus::Lecture);
        }
        match self.mode {
            Mode::Blanche => {
                if dans(&self.blanche, &hote) || google_national(&hote) {
                    Ok(())
                } else {
                    Err(Refus::HorsBlanche)
                }
            }
            Mode::Noire => {
                if suffixes(&hote).any(|d| self.noire.contient(d)) {
                    Err(Refus::Noire)
                } else {
                    Ok(())
                }
            }
        }
    }
}

/// Les domaines d'un fichier : un par ligne, `#` en commentaire, le `www.` et le point de
/// fin enlevés. On accepte aussi les lignes d'un fichier `hosts` (« 0.0.0.0 domaine »).
pub fn domaines(texte: &str) -> HashSet<String> {
    let mut set = HashSet::new();
    chaque_domaine(texte, |d| {
        set.insert(d.to_string());
    });
    set
}

/// Chaque domaine d'un fichier de liste (voir `domaines`), sans tout garder en mémoire.
fn chaque_domaine(texte: &str, mut f: impl FnMut(&str)) {
    for ligne in texte.lines() {
        let ligne = ligne.split('#').next().unwrap_or("").trim();
        if ligne.is_empty() {
            continue;
        }
        // « 0.0.0.0 domaine » ou « 127.0.0.1 domaine » : on garde le dernier morceau.
        let mot = ligne.split_whitespace().next_back().unwrap_or("");
        let mot = mot.trim_start_matches("www.").trim_matches('.').trim();
        let mot = mot.to_ascii_lowercase();
        if mot.contains('.') && !adresse_chiffree(&mot) {
            f(&mot);
        }
    }
}

/// L'hôte d'une adresse `http`/`https`, en minuscules, sans le port, sans ce qui précède
/// un `@` (`https://www.youtube.com@mechant.example/` mène chez le méchant) et sans le
/// point final. `None` si le schéma n'est pas `http` ou `https`.
pub fn hote(url: &str) -> Option<String> {
    let (schema, reste) = url.split_once("://")?;
    let schema = schema.trim().to_ascii_lowercase();
    if schema != "http" && schema != "https" {
        return None;
    }
    let autorite = reste
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .rsplit('@')
        .next()
        .unwrap_or("");
    // IPv6 entre crochets : refusé plus loin (adresse chiffrée).
    let hote = if let Some(fin) = autorite.strip_prefix('[') {
        format!("[{}", fin.split(']').next().unwrap_or(""))
    } else {
        autorite.split(':').next().unwrap_or("").to_string()
    };
    Some(hote.trim_matches('.').to_ascii_lowercase())
}

/// Une adresse écrite en chiffres (`192.168.1.1`, `[::1]`, `2130706433`) : jamais sur un
/// écran, c'est la porte d'entrée vers le routeur et les services de la machine.
fn adresse_chiffree(hote: &str) -> bool {
    hote.starts_with('[')
        || hote.contains(':')
        || !hote.is_empty() && hote.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// L'hôte, ou un de ses domaines parents, est-il dans la liste ?
/// (v208) Les pages de connexion à un compte Google ou YouTube (`accounts.google.com`,
/// `accounts.google.fr`, `myaccount.google.com`, `accounts.youtube.com`…).
fn connexion(hote: &str) -> bool {
    let Some((premier, reste)) = hote.split_once('.') else {
        return false;
    };
    matches!(premier, "accounts" | "myaccount")
        && (reste == "youtube.com" || reste == "google.com" || google_national(reste))
}

fn dans(set: &HashSet<String>, hote: &str) -> bool {
    suffixes(hote).any(|d| set.contains(d))
}

/// L'hôte puis ses domaines parents qui ont encore un point (`a.b.exemple.fr`,
/// `b.exemple.fr`, `exemple.fr`).
fn suffixes(hote: &str) -> impl Iterator<Item = &str> {
    std::iter::successors(Some(hote), |h| h.split_once('.').map(|(_, p)| p))
        .filter(|d| d.contains('.'))
}

/// (v243) L'hôte est-il ce domaine ou un de ses sous-domaines ?
fn sous(hote: &str, domaine: &str) -> bool {
    hote == domaine
        || hote
            .strip_suffix(domaine)
            .is_some_and(|r| r.ends_with('.'))
}

/// `google.fr`, `www.google.co.uk`… : Google renvoie souvent vers le domaine du pays, et
/// on ne va pas lister deux cents domaines dans la liste blanche. Seul `google.<pays>`
/// passe : `google.mechant.example` ne finit pas par un domaine de pays, et
/// `mechantgoogle.fr` n'est pas `google.fr`.
fn google_national(hote: &str) -> bool {
    let sans = hote
        .strip_prefix("www.")
        .or_else(|| hote.strip_prefix("consent."))
        .unwrap_or(hote);
    let Some(apres) = sans.strip_prefix("google.") else {
        return false;
    };
    let bouts: Vec<&str> = apres.split('.').collect();
    let court = |s: &&str| s.len() >= 2 && s.len() <= 3 && s.chars().all(|c| c.is_ascii_alphabetic());
    match bouts.as_slice() {
        [pays] => court(pays),
        [sous, pays] => court(sous) && court(pays),
        _ => false,
    }
}

/// Mode restreint de YouTube, tel que Google le documente pour les réseaux encadrés
/// (en-tête `YouTube-Restrict`). (v205)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Restreint {
    Strict,
    Modere,
    Non,
}

impl Restreint {
    pub fn from_name(name: &str) -> Option<Restreint> {
        match name.trim().to_ascii_lowercase().as_str() {
            "strict" => Some(Restreint::Strict),
            "modere" | "moderate" => Some(Restreint::Modere),
            "non" | "off" => Some(Restreint::Non),
            _ => None,
        }
    }

    /// La valeur de l'en-tête, s'il y en a une.
    pub fn entete(self) -> Option<&'static str> {
        match self {
            Restreint::Strict => Some("Strict"),
            Restreint::Modere => Some("Moderate"),
            Restreint::Non => None,
        }
    }

    /// L'hôte vers lequel Google demande de faire pointer les noms de YouTube pour le même
    /// effet par le DNS (ce que fait `--host-resolver-rules`).
    pub fn hote_dns(self) -> Option<&'static str> {
        match self {
            Restreint::Strict => Some("restrict.youtube.com"),
            Restreint::Modere => Some("restrictmoderate.youtube.com"),
            Restreint::Non => None,
        }
    }
}

/// Les noms que Google dit de rediriger pour le mode restreint — et aucun autre : donner
/// un CNAME à `youtube.com` ou à `s.ytimg.com` casse YouTube.
pub const HOTES_YOUTUBE: &[&str] = &[
    "www.youtube.com",
    "m.youtube.com",
    "youtubei.googleapis.com",
    "youtube.googleapis.com",
    "www.youtube-nocookie.com",
];

/// Faut-il mettre l'en-tête `YouTube-Restrict` sur une requête vers cet hôte ?
/// (v210) La page du lecteur officiel de YouTube sur les écrans (`lecteur.html`), servie
/// par le navigateur lui-même à cette adresse — jamais demandée au réseau. C'est l'identité
/// du jeu (le compte GitHub de son auteur) : YouTube la reçoit comme origine du lecteur.
pub const LECTEUR: &str = "https://viendax.github.io/minelib/lecteur";

/// L'adresse est-elle celle de la page du lecteur ?
pub fn est_lecteur(url: &str) -> bool {
    url.strip_prefix(LECTEUR)
        .is_some_and(|r| r.is_empty() || r.starts_with('?') || r.starts_with('#'))
}

/// (v210) Une vidéo du site YouTube (`/watch?v=…`, `/shorts/…`, `/live/…`, `youtu.be/…`) :
/// la page du lecteur qui la joue, au même temps. Le site complet ne sert qu'à chercher.
pub fn vers_lecteur(url: &str) -> Option<String> {
    let reste = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let reste = reste.split('#').next()?;
    let (hote, chemin) = reste.split_once('/').unwrap_or((reste, ""));
    let (chemin, requete) = chemin.split_once('?').unwrap_or((chemin, ""));
    let parametre = |nom: &str| {
        requete
            .split('&')
            .find_map(|kv| kv.strip_prefix(nom).and_then(|v| v.strip_prefix('=')))
    };
    // (v243) Le lecteur embarqué a renvoyé la vidéo au site (YouTube ne la permet pas
    // ailleurs : limitée par l'âge, ou intégration coupée par son auteur).
    if parametre("mlb") == Some("site") {
        return None;
    }
    let id = match hote.to_ascii_lowercase().as_str() {
        "youtu.be" | "www.youtu.be" => chemin.split('/').next()?,
        "www.youtube.com" | "youtube.com" | "m.youtube.com" => {
            if chemin == "watch" {
                parametre("v")?
            } else {
                ["shorts/", "live/"]
                    .iter()
                    .find_map(|p| chemin.strip_prefix(p))?
                    .split('/')
                    .next()?
            }
        }
        _ => return None,
    };
    let ok = id.len() == 11
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if !ok {
        return None;
    }
    let t = parametre("t").or_else(|| parametre("start")).map_or(0, secondes);
    Some(if t > 0 {
        format!("{LECTEUR}?v={id}&t={t}")
    } else {
        format!("{LECTEUR}?v={id}")
    })
}

/// `90`, `90s`, `1m30s`, `1h2m3s` : en secondes (0 si illisible).
fn secondes(t: &str) -> u32 {
    let mut total: u32 = 0;
    let mut n: u32 = 0;
    let mut chiffres = false;
    for c in t.chars() {
        match c {
            '0'..='9' => {
                n = n.saturating_mul(10).saturating_add(c as u32 - '0' as u32);
                chiffres = true;
            }
            'h' | 'm' | 's' if chiffres => {
                let k = match c {
                    'h' => 3600,
                    'm' => 60,
                    _ => 1,
                };
                total = total.saturating_add(n.saturating_mul(k));
                n = 0;
                chiffres = false;
            }
            _ => return 0,
        }
    }
    total.saturating_add(n).min(360_000)
}

pub fn youtube(hote: &str) -> bool {
    HOTES_YOUTUBE.iter().any(|h| *h == hote)
        || hote == "youtube.com"
        || hote.ends_with(".youtube.com")
}

/// Une recherche Google : il faut y ajouter `safe=active` (ce que fait le réglage
/// « SafeSearch » imposé par un administrateur).
pub fn recherche_google(url: &str) -> bool {
    let Some(h) = hote(url) else {
        return false;
    };
    if !(h == "google.com" || h.ends_with(".google.com") || google_national(&h)) {
        return false;
    }
    let chemin = url
        .split_once("://")
        .map(|(_, r)| r)
        .unwrap_or(url)
        .split_once('/')
        .map(|(_, c)| c)
        .unwrap_or("");
    let chemin = chemin.split(['?', '#']).next().unwrap_or("");
    chemin.is_empty() || chemin == "search" || chemin.starts_with("search")
}

/// La même adresse avec `safe=active`, si elle ne l'avait pas déjà.
pub fn avec_safe(url: &str) -> Option<String> {
    let (avant, apres) = match url.split_once('#') {
        Some((a, b)) => (a.to_string(), format!("#{b}")),
        None => (url.to_string(), String::new()),
    };
    if avant.contains("safe=active") {
        return None;
    }
    let avant = avant
        .replace("&safe=off", "")
        .replace("?safe=off&", "?")
        .replace("&safe=high", "")
        .replace("&safe=medium", "");
    let sep = if avant.contains('?') { '&' } else { '?' };
    Some(format!("{avant}{sep}safe=active{apres}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blanche() -> Filtre {
        Filtre::charge(Mode::Blanche, None).0
    }

    fn noire(liste: &str) -> Filtre {
        Filtre {
            mode: Mode::Noire,
            blanche: HashSet::new(),
            noire: {
                let mut e = Empreintes::default();
                e.ajoute(liste);
                e.range();
                e
            },
            sans_compte: false,
        }
    }

    #[test]
    fn la_liste_blanche_ouvre_les_sites_pour_enfants_et_rien_de_plus() {
        let f = blanche();
        for ok in [
            "https://www.lumni.fr/",
            "https://fr.vikidia.org/wiki/Chat",
            "https://www.nasa.gov/",
            "https://www.pbskids.org/",
            "https://galhia.eklablog.com/",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
        ] {
            assert_eq!(f.juge(ok), Ok(()), "{ok}");
        }
        assert_eq!(f.juge("https://www.twitch.tv/"), Err(Refus::Lecture));
        for non in [
            "https://fr.wikipedia.org/",
            "https://www.dailymotion.com/",
            "https://archive.org/",
            "https://eklablog.com/",
            "https://autre.eklablog.com/",
        ] {
            assert_eq!(f.juge(non), Err(Refus::HorsBlanche), "{non}");
        }
    }

    #[test]
    fn une_video_du_site_s_ouvre_dans_le_lecteur() {
        let l = |id: &str| format!("{LECTEUR}?v={id}");
        let x = "dQw4w9WgXcQ";
        assert_eq!(vers_lecteur(&format!("https://www.youtube.com/watch?v={x}")), Some(l(x)));
        assert_eq!(
            vers_lecteur(&format!("https://www.youtube.com/watch?list=PL1&v={x}&t=1m30s")),
            Some(format!("{}&t=90", l(x)))
        );
        assert_eq!(vers_lecteur(&format!("https://m.youtube.com/shorts/{x}")), Some(l(x)));
        assert_eq!(vers_lecteur(&format!("https://youtu.be/{x}?t=42")), Some(format!("{}&t=42", l(x))));
        assert_eq!(vers_lecteur(&format!("https://www.youtube.com/live/{x}")), Some(l(x)));
        // La recherche, l'accueil, une chaîne et le lecteur embarqué restent où ils sont.
        for reste in [
            "https://www.youtube.com/",
            "https://www.youtube.com/results?search_query=chat",
            "https://www.youtube.com/@chaine",
            "https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ",
            "https://www.youtube.com/embed/dQw4w9WgXcQ",
            "https://www.youtube.com/watch?v=court",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ&mlb=site",
            "https://www.youtube.com.mechant.example/watch?v=dQw4w9WgXcQ",
        ] {
            assert_eq!(vers_lecteur(reste), None, "{reste}");
        }
        assert_eq!(secondes("abc"), 0);
        assert_eq!(secondes("1h2m3s"), 3723);
    }

    #[test]
    fn la_page_du_lecteur_s_ouvre_partout_mais_pas_le_reste_du_domaine() {
        let f = blanche().sans_compte();
        assert_eq!(f.juge(&format!("{LECTEUR}?v=dQw4w9WgXcQ")), Ok(()));
        assert!(est_lecteur(LECTEUR));
        assert!(!est_lecteur(&format!("{LECTEUR}-faux")));
        assert_eq!(f.juge("https://viendax.github.io/autre"), Err(Refus::HorsBlanche));
        assert_eq!(f.juge(&format!("{LECTEUR}-faux")), Err(Refus::HorsBlanche));
    }

    #[test]
    fn en_ligne_aucune_connexion_a_un_compte() {
        let solo = blanche();
        let en_ligne = blanche().sans_compte();
        for url in [
            "https://accounts.google.com/ServiceLogin?service=youtube",
            "https://accounts.google.fr/",
            "https://myaccount.google.com/",
            "https://accounts.youtube.com/accounts/SetSID",
        ] {
            assert_ne!(solo.juge(url), Err(Refus::Compte), "{url}");
            assert_eq!(en_ligne.juge(url), Err(Refus::Compte), "{url}");
        }
        assert_eq!(en_ligne.juge("https://www.youtube.com/"), Ok(()));
        assert_eq!(en_ligne.juge("https://www.google.com/search?q=chat"), Ok(()));
    }

    #[test]
    fn only_the_web_opens_on_a_screen() {
        let f = blanche();
        for mauvais in [
            "file:///C:/Users/viendax/Documents/secret.txt",
            "FILE://C:/x",
            "chrome://settings",
            "about:config",
            "javascript:alert(1)",
            "data:text/html,<h1>x",
            "blob:https://www.youtube.com/1234",
            "ftp://ftp.example.com/x",
            "view-source:https://www.youtube.com",
        ] {
            assert_eq!(f.juge(mauvais), Err(Refus::Schema), "{mauvais}");
        }
        assert_eq!(f.juge("about:blank"), Ok(()));
        assert_eq!(f.juge(""), Ok(()));
    }

    #[test]
    fn the_local_network_stays_out_of_reach() {
        let f = noire("");
        for mauvais in [
            "http://localhost:8080/",
            "http://127.0.0.1/",
            "https://192.168.1.1/admin",
            "http://10.0.0.5/",
            "http://172.16.3.4/",
            "http://[::1]/",
            "https://[fe80::1]/",
            "http://2130706433/",
            "http://routeur/",
            "http://NAS/partage",
        ] {
            assert_eq!(f.juge(mauvais), Err(Refus::Privee), "{mauvais}");
        }
    }

    #[test]
    fn the_allowlist_opens_youtube_and_google_and_nothing_else() {
        let f = blanche();
        for bon in [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://youtube.com/",
            "https://m.youtube.com/",
            "https://i.ytimg.com/vi/x/hqdefault.jpg",
            "https://rr3---sn-abcd.googlevideo.com/videoplayback?x=1",
            "https://www.google.com/search?q=chat",
            "https://www.google.fr/",
            "https://consent.google.fr/",
            "https://www.google.co.uk/search?q=x",
            "https://fonts.gstatic.com/x.woff2",
            // Les publicités de YouTube doivent passer (condition de YouTube).
            "https://pagead2.googlesyndication.com/x",
            "https://ad.doubleclick.net/x",
        ] {
            assert_eq!(f.juge(bon), Ok(()), "{bon}");
        }
        for mauvais in [
            "https://exemple-porno.example/",
            "https://google.mechant.example/",
            "https://mechantgoogle.fr/",
            "https://youtube.com.mechant.example/",
            "https://www.youtube.com.mechant.example/watch?v=x",
        ] {
            assert_eq!(f.juge(mauvais), Err(Refus::HorsBlanche), "{mauvais}");
        }
    }

    #[test]
    fn a_name_before_an_at_sign_does_not_open_the_door() {
        let f = blanche();
        // L'hôte est ce qui suit le dernier `@`, pas ce qui le précède.
        assert_eq!(
            f.juge("https://www.youtube.com@mechant.example/x"),
            Err(Refus::HorsBlanche)
        );
        assert_eq!(
            f.juge("https://user:pass@www.youtube.com/watch?v=x"),
            Ok(())
        );
        assert_eq!(hote("https://WWW.YouTube.COM:443/x").as_deref(), Some("www.youtube.com"));
        assert_eq!(hote("https://www.youtube.com./x").as_deref(), Some("www.youtube.com"));
    }

    #[test]
    fn the_blocklist_catches_a_domain_and_all_its_subdomains() {
        let f = noire("# commentaire\nmechant.example\n0.0.0.0 autre.example\nwww.troisieme.example\n");
        for mauvais in [
            "https://mechant.example/",
            "https://www.mechant.example/x",
            "https://cdn.images.mechant.example/x.jpg",
            "https://autre.example/",
            "https://troisieme.example/",
            "https://a.troisieme.example/",
        ] {
            assert_eq!(f.juge(mauvais), Err(Refus::Noire), "{mauvais}");
        }
        for bon in [
            "https://example.com/",
            "https://pasmechant.example/",
            "https://www.youtube.com/",
        ] {
            assert_eq!(f.juge(bon), Ok(()), "{bon}");
        }
    }

    /// (v243) Sans fichier, la liste noire intégrée suffit : le web s'ouvre, sauf les
    /// interdits ; les opérateurs de jeux agréés en France (ARJEL) restent ouverts.
    #[test]
    fn without_a_blocklist_file_the_built_in_one_is_used() {
        let (f, mot) = Filtre::charge(Mode::Noire, Some(Path::new("/dossier/qui/n/existe/pas")));
        assert_eq!(f.mode, Mode::Noire);
        assert!(f.noire.len() > 200_000, "{mot}");
        for bon in [
            "https://fr.wikipedia.org/wiki/Chat",
            "https://www.lemonde.fr/",
            "https://github.com/",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://www.winamax.fr/",
        ] {
            assert_eq!(f.juge(bon), Ok(()), "{bon}");
        }
        for mauvais in [
            "https://www.pokerstars.com/",
            "https://www.888casino.com/",
            "https://4chan.org/",
            "https://unblocktheweb.appspot.com/",
        ] {
            assert_eq!(f.juge(mauvais), Err(Refus::Noire), "{mauvais}");
        }
        for plateforme in ["https://www.twitch.tv/x", "https://player.vimeo.com/video/1", "https://vimeo.com/1"] {
            assert_eq!(f.juge(plateforme), Err(Refus::Lecture), "{plateforme}");
        }
        assert_eq!(f.juge("https://nottwitch.tv/"), Ok(()));
    }

    #[test]
    fn the_daily_blocklist_adds_to_the_built_in_one() {
        let d = std::env::temp_dir().join(format!("minelib-noire-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("noire_ut1.txt"), "# du jour\nnouveau-mechant.example\n").unwrap();
        std::fs::write(d.join("liste_noire.txt"), "a-la-main.example\n").unwrap();
        let (f, mot) = Filtre::charge(Mode::Noire, Some(&d));
        std::fs::remove_dir_all(&d).ok();
        assert!(mot.contains("du jour : 1 domaines"), "{mot}");
        assert_eq!(f.juge("https://cdn.nouveau-mechant.example/x"), Err(Refus::Noire));
        assert_eq!(f.juge("https://a-la-main.example/"), Err(Refus::Noire));
        assert_eq!(f.juge("https://www.pokerstars.com/"), Err(Refus::Noire));
        assert_eq!(f.juge("https://exemple.com/"), Ok(()));
    }

    #[test]
    fn the_restricted_mode_header_goes_only_to_the_right_hosts() {
        assert_eq!(Restreint::Strict.entete(), Some("Strict"));
        assert_eq!(Restreint::Non.entete(), None);
        for bon in [
            "www.youtube.com",
            "m.youtube.com",
            "youtube.com",
            "youtubei.googleapis.com",
        ] {
            assert!(youtube(bon), "{bon}");
        }
        for mauvais in ["s.ytimg.com", "youtu.be", "googleapis.com", "google.com"] {
            assert!(!youtube(mauvais), "{mauvais}");
        }
    }

    #[test]
    fn a_google_search_always_ends_up_with_safesearch() {
        assert!(recherche_google("https://www.google.com/search?q=x"));
        assert!(recherche_google("https://www.google.fr/"));
        assert!(!recherche_google("https://www.youtube.com/results?search_query=x"));
        assert_eq!(
            avec_safe("https://www.google.com/search?q=x").as_deref(),
            Some("https://www.google.com/search?q=x&safe=active")
        );
        assert_eq!(
            avec_safe("https://www.google.com/").as_deref(),
            Some("https://www.google.com/?safe=active")
        );
        // Déjà mis : on ne touche plus à l'adresse (sinon on rechargerait sans fin).
        assert_eq!(avec_safe("https://www.google.com/search?q=x&safe=active"), None);
        // Quelqu'un qui demande « safe=off » se le fait enlever.
        assert_eq!(
            avec_safe("https://www.google.com/search?q=x&safe=off").as_deref(),
            Some("https://www.google.com/search?q=x&safe=active")
        );
        // L'ancre reste à la fin.
        assert_eq!(
            avec_safe("https://www.google.com/search?q=x#bas").as_deref(),
            Some("https://www.google.com/search?q=x&safe=active#bas")
        );
    }
}
