//! (v307) Journaux réseau reçus des joueurs, relevés et alertes du relais.
//!
//! Quand l'hôte de ce relais l'a demandé (`set_receive_logs` : paramètres avancés du jeu,
//! ou `--journaux` pour le relais seul), le relais range dans `journaux_reseau.txt`, dans
//! son dossier :
//!
//! - les lignes réseau que lui envoient les joueurs qui ont accepté de les partager
//!   (`Request::NetLog`, voir `minelib_net::netlog`), sous un numéro tiré au hasard par
//!   connexion : on peut suivre une session, pas savoir de qui elle est ;
//! - son propre relevé de santé, une fois par minute ;
//! - une **alerte** quand ce relevé ressemble à une tentative de le faire tomber : rafale
//!   de mots de passe faux, file des vérifications pleine, positions envoyées en masse,
//!   annonces ou demandes refusées en nombre, bond du nombre de connexions.
//!
//! Rien n'est rangé si l'hôte n'a rien demandé, et le relais ne dit alors à personne
//! d'envoyer quoi que ce soit.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use minelib_net::proto::Response;
use minelib_net::transport;

use super::State;

static RECEIVE: AtomicBool = AtomicBool::new(false);

/// Lignes acceptées d'une connexion par minute.
const LINES_PER_MIN: u16 = 60;
/// Le fichier est recommencé au-delà de cette taille (l'ancien est gardé à côté).
const FILE_MAX: u64 = 20 * 1024 * 1024;
pub(crate) const FILE: &str = "journaux_reseau.txt";

/// L'hôte de ce relais reçoit (ou non) les journaux réseau des joueurs.
pub fn set_receive_logs(on: bool) {
    RECEIVE.store(on, Ordering::Relaxed);
}

pub(crate) fn receiving() -> bool {
    RECEIVE.load(Ordering::Relaxed)
}

/// Une connexion vient d'entrer : si l'hôte reçoit les journaux, elle le sait.
pub(crate) fn tell(st: &State, id: u64) {
    if receiving() {
        if let Some(c) = st.conns.get(&id) {
            transport::push(&c.tx, &Response::NetLogWanted { on: true });
        }
    }
}

/// À chaque battement : l'hôte a changé d'avis, tous ceux qui sont entrés le savent.
pub(crate) fn upkeep(st: &mut State) {
    let on = receiving();
    if on == st.fast.logs_said {
        return;
    }
    st.fast.logs_said = on;
    for c in st.conns.values().filter(|c| c.pseudo.is_some()) {
        transport::push(&c.tx, &Response::NetLogWanted { on });
    }
}

/// Numéro d'une connexion dans le fichier : sans rapport avec son compte ni son adresse,
/// et différent à chaque lancement du relais.
fn tag(id: u64) -> String {
    use std::hash::{BuildHasher, Hasher};
    static SALT: std::sync::OnceLock<std::collections::hash_map::RandomState> =
        std::sync::OnceLock::new();
    let mut h = SALT.get_or_init(Default::default).build_hasher();
    h.write_u64(id);
    format!("{:04x}", h.finish() & 0xffff)
}

/// `Request::NetLog` : lignes réseau d'un joueur.
pub(crate) fn got(st: &mut State, id: u64, lines: Vec<String>) {
    if !receiving() {
        return;
    }
    let r = st.fast.log_rate.entry(id).or_insert((Instant::now(), 0));
    if r.0.elapsed() >= std::time::Duration::from_secs(60) {
        *r = (Instant::now(), 0);
    }
    let who = tag(id);
    for line in lines {
        if r.1 >= LINES_PER_MIN {
            st.fast.refused[2] += 1;
            continue;
        }
        r.1 += 1;
        if let Some(line) = minelib_net::netlog::clean(&line) {
            append(&st.cfg.dir, &who, &line);
        }
    }
}

/// Une ligne du relais lui-même (relevé de santé, alerte).
pub(crate) fn own(st: &State, line: &str) {
    if receiving() {
        append(&st.cfg.dir, "relais", line);
    }
}

fn append(dir: &std::path::Path, who: &str, line: &str) {
    let path = dir.join(FILE);
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > FILE_MAX) {
        let _ = std::fs::rename(&path, dir.join("journaux_reseau_ancien.txt"));
    }
    let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let _ = writeln!(f, "{} [{who}] {line}", date(now));
}

/// Date et heure UTC lisibles d'un nombre de secondes depuis 1970.
fn date(secs: u64) -> String {
    let (days, rest) = (secs / 86_400, secs % 86_400);
    // (Jours → année, mois, jour : calendrier grégorien, méthode de H. Hinnant.)
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// Ce qui, dans le relevé d'une minute, ressemble à une tentative de faire tomber le
/// relais (rien : tout va bien).
pub(crate) fn alerts(
    logins_bad: u32,
    logins_busy: u32,
    pos_dropped: u64,
    refused: [u32; 3],
    conns: (usize, usize),
) -> Vec<String> {
    let mut a = Vec::new();
    if logins_bad >= 20 {
        a.push(format!(
            "{logins_bad} connexions de compte refusées (essais de mots de passe ?)"
        ));
    }
    if logins_busy >= 1 {
        a.push(format!(
            "{logins_busy} connexions refusées parce que trop de mots de passe attendaient \
             d'être vérifiés (on cherche à occuper le relais ?)"
        ));
    }
    if pos_dropped >= 3000 {
        a.push(format!(
            "{pos_dropped} positions écartées (un jeu en envoie bien plus que prévu)"
        ));
    }
    if refused[0] >= 20 {
        a.push(format!("{} annonces de passeur refusées (trop d'un coup)", refused[0]));
    }
    if refused[1] >= 100 {
        a.push(format!("{} demandes de tunnel refusées (trop d'un coup)", refused[1]));
    }
    if refused[2] >= 200 {
        a.push(format!("{} lignes de journal refusées (trop d'un coup)", refused[2]));
    }
    if conns.1 >= 40 && conns.1 >= conns.0 * 2 {
        a.push(format!(
            "connexions passées de {} à {} en une minute",
            conns.0, conns.1
        ));
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_read_well() {
        assert_eq!(date(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(date(951_782_400), "2000-02-29 00:00:00 UTC");
        assert_eq!(date(1_795_561_324), "2026-11-24 23:02:04 UTC");
    }

    /// Un relevé ordinaire ne donne aucune alerte ; chaque signe d'attaque en donne une.
    #[test]
    fn a_quiet_minute_raises_nothing_and_each_sign_of_attack_does() {
        assert!(alerts(3, 0, 120, [0, 5, 0], (10, 14)).is_empty());
        assert_eq!(alerts(25, 0, 0, [0; 3], (0, 0)).len(), 1);
        assert_eq!(alerts(0, 2, 0, [0; 3], (0, 0)).len(), 1);
        assert_eq!(alerts(0, 0, 5000, [0; 3], (0, 0)).len(), 1);
        assert_eq!(alerts(0, 0, 0, [30, 150, 300], (0, 0)).len(), 3);
        assert_eq!(alerts(0, 0, 0, [0; 3], (20, 45)).len(), 1);
        assert!(alerts(0, 0, 0, [0; 3], (20, 30)).is_empty());
    }
}
