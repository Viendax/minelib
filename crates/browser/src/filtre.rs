//! Ce qui peut s'ouvrir sur un écran en jeu, et rien d'autre. (v205)
//!
//! Le jeu est tout public : il doit être **impossible** de finir sur du porno ou sur un
//! site illégal depuis un écran. Trois barrières, dans cet ordre :
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
}

impl Refus {
    pub fn raison(self) -> &'static str {
        match self {
            Refus::Schema => "ce genre d'adresse ne s'ouvre pas sur un écran",
            Refus::Privee => "pas de machine du réseau local sur un écran",
            Refus::HorsBlanche => "site hors de la liste autorisée",
            Refus::Noire => "site filtré",
            Refus::Compte => "pas de connexion à un compte sur les écrans en ligne",
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

/// Le filtre d'un navigateur : son mode et ses deux listes.
#[derive(Debug, Clone)]
pub struct Filtre {
    mode: Mode,
    blanche: HashSet<String>,
    noire: HashSet<String>,
    /// (v208) En ligne : pas de page de connexion à un compte.
    sans_compte: bool,
}

impl Filtre {
    /// Le filtre demandé, avec les listes du dossier s'il y en a. Rend aussi ce qu'il faut
    /// dire au journal (pourquoi on n'a pas eu ce qui était demandé).
    pub fn charge(mode: Mode, dossier: Option<&Path>) -> (Filtre, String) {
        let lire = |nom: &str| -> Option<(HashSet<String>, usize)> {
            let d = dossier?;
            let texte = std::fs::read_to_string(d.join(nom)).ok()?;
            let set = domaines(&texte);
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
        let (noire, mode) = match (mode, lire("liste_noire.txt")) {
            (Mode::Noire, Some((set, n))) => {
                mot.push_str(&format!("liste noire : {n} domaines ; "));
                (set, Mode::Noire)
            }
            (Mode::Noire, None) => {
                mot.push_str("pas de fichier de liste noire : liste blanche à la place ; ");
                (HashSet::new(), Mode::Blanche)
            }
            (Mode::Blanche, autre) => (autre.map(|(s, _)| s).unwrap_or_default(), Mode::Blanche),
        };
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
        if self.sans_compte && connexion(&hote) {
            return Err(Refus::Compte);
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
                if dans(&self.noire, &hote) {
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
            set.insert(mot);
        }
    }
    set
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
    if set.contains(hote) {
        return true;
    }
    let mut reste = hote;
    while let Some((_, parent)) = reste.split_once('.') {
        if parent.contains('.') && set.contains(parent) {
            return true;
        }
        reste = parent;
    }
    false
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
            noire: domaines(liste),
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
        for non in [
            "https://fr.wikipedia.org/",
            "https://www.dailymotion.com/",
            "https://www.twitch.tv/",
            "https://archive.org/",
            "https://eklablog.com/",
            "https://autre.eklablog.com/",
        ] {
            assert_eq!(f.juge(non), Err(Refus::HorsBlanche), "{non}");
        }
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

    #[test]
    fn without_a_blocklist_file_we_fall_back_to_the_allowlist() {
        let (f, mot) = Filtre::charge(Mode::Noire, Some(Path::new("/dossier/qui/n/existe/pas")));
        assert_eq!(f.mode, Mode::Blanche);
        assert!(mot.contains("pas de fichier"), "{mot}");
        assert_eq!(f.juge("https://exemple-porno.example/"), Err(Refus::HorsBlanche));
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
