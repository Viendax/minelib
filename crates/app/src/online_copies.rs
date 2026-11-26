//! (v325) Copies en ligne des mondes de construction.
//!
//! Un monde ouvert en ligne se joue dans sa propre copie (`en_ligne/<horodatage>`), jamais
//! dans le monde solo dont il vient : ce qu'on y fait ne touche pas au monde solo, ni
//! l'inverse, et rouvrir ce monde solo en ligne en fait un autre serveur. Le registre
//! `en_ligne/copies.txt` dit quelle copie garde le dernier état de quel monde en ligne
//! (relais, clé du monde) : de quoi le réveiller tel qu'il s'est endormi.
//!
//! (Un invité garde, lui, sa copie dans la carte téléchargée (`maps/`) ; s'il devient le
//! propriétaire et part le dernier, c'est elle que le registre retient.)

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Dossier des copies, dans le dossier du jeu.
pub const DIR: &str = "en_ligne";
const INDEX: &str = "copies.txt";
/// Une copie qu'aucun monde en ligne ne retient et où l'on n'a pas joué (le relais n'a
/// jamais répondu, le jeu s'est fermé aussitôt) est supprimée au lancement, passé ce délai
/// (un autre jeu lancé sur ce PC peut être en train d'en ouvrir une).
const ORPHAN_S: u64 = 24 * 3600;
/// Enregistrée moins de ce temps après sa copie : on n'y a pas joué.
const UNTOUCHED_S: u64 = 120;

#[derive(Clone, Debug, PartialEq)]
struct Line {
    relay: String,
    world: String,
    /// Relatif au dossier du jeu, séparé par `/`.
    dir: String,
    saved: u64,
}

fn index(root: &Path) -> PathBuf {
    root.join(DIR).join(INDEX)
}

fn read(root: &Path) -> Vec<Line> {
    fs::read_to_string(index(root))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            Some(Line {
                relay: f.next()?.to_string(),
                world: f.next()?.to_string(),
                dir: f.next()?.to_string(),
                saved: f.next()?.parse().ok()?,
            })
        })
        .collect()
}

fn write(root: &Path, lines: &[Line]) {
    let text: String = lines
        .iter()
        .map(|l| format!("{}\t{}\t{}\t{}\n", l.relay, l.world, l.dir, l.saved))
        .collect();
    let path = index(root);
    let tmp = path.with_extension("tmp");
    let done = fs::create_dir_all(root.join(DIR))
        .and_then(|()| fs::write(&tmp, text))
        .and_then(|()| fs::rename(&tmp, &path));
    if let Err(e) = done {
        log::warn!("registre des copies en ligne : {e}");
    }
}

fn rel(root: &Path, dir: &Path) -> Option<String> {
    let r = dir.strip_prefix(root).ok()?;
    let parts: Vec<String> = r
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    (!parts.is_empty() && parts.iter().all(|p| !p.contains(['\t', '\n', '/', '\\'])))
        .then(|| parts.join("/"))
}

fn abs(root: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(root.to_path_buf(), |p, c| p.join(c))
}

/// Une copie en ligne (sous `en_ligne/`) : à nous, on peut la supprimer. Ni un monde solo,
/// ni une carte téléchargée.
pub fn is_copy(root: &Path, dir: &Path) -> bool {
    dir.parent() == Some(root.join(DIR).as_path())
}

/// Nouvelle copie du monde `src` (un monde solo, une carte téléchargée) : `en_ligne/<n>`.
/// Copiée à part puis renommée : jamais une copie à moitié faite à sa place.
pub fn fork(root: &Path, src: &Path) -> Result<PathBuf> {
    let base = root.join(DIR);
    fs::create_dir_all(&base).with_context(|| format!("création de {}", base.display()))?;
    let stamp = crate::save::now();
    let mut n = 0u32;
    let dir = loop {
        let name = if n == 0 {
            stamp.to_string()
        } else {
            format!("{stamp}-{n}")
        };
        let d = base.join(&name);
        if !d.exists() && !base.join(format!(".{name}")).exists() {
            break d;
        }
        n += 1;
    };
    let name = dir
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let tmp = base.join(format!(".{name}"));
    if let Err(e) = crate::fpsmap::copy_dir(src, &tmp) {
        let _ = fs::remove_dir_all(&tmp);
        return Err(e.context(format!("copie de {}", src.display())));
    }
    fs::rename(&tmp, &dir).with_context(|| format!("copie vers {}", dir.display()))?;
    Ok(dir)
}

/// La copie qui garde le monde en ligne `world` (du relais `relay` de préférence), si elle
/// existe encore.
pub fn find(root: &Path, relay: &str, world: &str) -> Option<PathBuf> {
    let lines = read(root);
    let pick = |same: bool| {
        lines
            .iter()
            .filter(|l| l.world == world && (!same || l.relay == relay))
            .map(|l| (l.saved, abs(root, &l.dir)))
            .filter(|(_, d)| d.join("level.txt").exists())
            .max_by_key(|(t, _)| *t)
            .map(|(_, d)| d)
    };
    pick(true).or_else(|| pick(false))
}

/// `dir` garde maintenant le dernier état du monde en ligne `world` du relais `relay`. Une
/// copie par monde : celle d'avant, sous `en_ligne/`, est supprimée (une carte téléchargée
/// reste où elle est).
pub fn keep(root: &Path, relay: &str, world: &str, dir: &Path) {
    let bad = |t: &str| t.is_empty() || t.contains(['\t', '\n', '\r']);
    if bad(world) || relay.contains(['\t', '\n', '\r']) {
        log::warn!("copie en ligne : clé de monde inutilisable ({world:?})");
        return;
    }
    let Some(here) = rel(root, dir) else {
        log::warn!("copie en ligne hors du dossier du jeu : {}", dir.display());
        return;
    };
    let mut lines = read(root);
    let mut before = Vec::new();
    lines.retain(|l| {
        let same = l.world == world && l.relay == relay;
        if same && l.dir != here {
            before.push(l.dir.clone());
        }
        !same
    });
    lines.push(Line {
        relay: relay.to_string(),
        world: world.to_string(),
        dir: here,
        saved: crate::save::now(),
    });
    write(root, &lines);
    for d in before {
        let p = abs(root, &d);
        if is_copy(root, &p) && !lines.iter().any(|l| l.dir == d) {
            log::info!("copie en ligne remplacée : {}", p.display());
            let _ = fs::remove_dir_all(&p);
        }
    }
}

/// Le monde en ligne `world` n'existe plus (son créateur l'a supprimé) : sa copie non plus
/// (une carte téléchargée reste où elle est).
pub fn forget(root: &Path, relay: &str, world: &str) {
    let (gone, rest): (Vec<Line>, Vec<Line>) = read(root)
        .into_iter()
        .partition(|l| l.world == world && (relay.is_empty() || l.relay == relay));
    if gone.is_empty() {
        return;
    }
    write(root, &rest);
    for l in gone {
        let p = abs(root, &l.dir);
        if is_copy(root, &p) && !rest.iter().any(|r| r.dir == l.dir) {
            log::info!("monde en ligne supprimé : sa copie aussi ({})", p.display());
            let _ = fs::remove_dir_all(&p);
        }
    }
}

/// Copie où l'on a joué sans que le monde soit jamais ouvert en ligne (le relais n'a pas
/// répondu) : elle devient un monde solo, « <nom> (pas ouvert en ligne) », plutôt que de
/// rester où personne ne la verrait. Rend son dossier.
pub fn to_solo(root: &Path, saves: &Path, dir: &Path) -> Result<PathBuf> {
    to_solo_as(root, saves, dir, "pas ouvert en ligne")
}

/// (v349) Le relais `relay` a oublié le monde endormi `w` pour faire de la place (personne n'y
/// avait joué depuis longtemps). La copie qu'en garde ce PC devient un monde solo, « <nom>
/// (oublié par le relais) », complété de la clé de son terrain (et de sa graine, chez son
/// créateur) : de quoi y jouer, et le rouvrir en ligne (« Ouvrir en ligne » en fait un
/// nouveau serveur). Une carte téléchargée reste où elle est, complétée de même. Rend où est
/// la copie ; rien si ce PC n'en a pas.
pub fn forgotten(
    root: &Path,
    saves: &Path,
    relay: &str,
    w: &minelib_net::proto::ForgottenWorld,
) -> Option<PathBuf> {
    let (gone, rest): (Vec<Line>, Vec<Line>) = read(root)
        .into_iter()
        .partition(|l| l.world == w.world && l.relay == relay);
    let latest = gone.iter().max_by_key(|l| l.saved)?;
    write(root, &rest);
    let dir = abs(root, &latest.dir);
    if !dir.join("level.txt").exists() {
        return None;
    }
    let dir = if is_copy(root, &dir) && !rest.iter().any(|r| r.dir == latest.dir) {
        match to_solo_as(root, saves, &dir, "oublié par le relais") {
            Ok(d) => d,
            Err(e) => {
                log::warn!("copie d'un monde oublié par le relais : {e:#}");
                dir
            }
        }
    } else {
        dir
    };
    if let Err(e) = give_seed(&dir, w.terrain, w.seed) {
        log::warn!("copie d'un monde oublié par le relais, sa graine : {e:#}");
    }
    Some(dir)
}

/// (v349) La graine qu'une copie n'avait pas (le relais la gardait) : écrite dans son
/// `level.txt`. Un monde créé depuis la v330 prend la clé de son terrain (`cle=`, et sa graine
/// publique `seed=` pour sa météo), et sa graine (`graine=`) si on la connaît ; un monde
/// d'avant, sa graine (`seed=`, qui fait son terrain).
fn give_seed(dir: &Path, terrain: u64, seed: u64) -> Result<()> {
    if terrain == 0 && seed == 0 {
        return Ok(());
    }
    let path = dir.join("level.txt");
    let text = fs::read_to_string(&path)?;
    let v2 = text.lines().any(|l| l.trim() == "generator=procedural2");
    let mut out: String = text
        .lines()
        .filter(|l| !(l.starts_with("seed=") || l.starts_with("cle=") || l.starts_with("graine=")))
        .map(|l| format!("{l}\n"))
        .collect();
    if v2 {
        let key = if terrain != 0 {
            terrain
        } else {
            minelib_net::graine::terrain_key(seed)
        };
        out += &format!(
            "seed={}\ncle={key:016x}\n",
            minelib_net::graine::public_seed(key)
        );
        if seed != 0 {
            out += &format!("graine={seed}\n");
        }
    } else {
        out += &format!(
            "seed={}\n",
            if terrain != 0 { terrain } else { seed } as u32
        );
    }
    fs::write(&path, out)?;
    Ok(())
}

/// Une copie devient un monde solo, « <nom> (`why`) ».
fn to_solo_as(root: &Path, saves: &Path, dir: &Path, why: &str) -> Result<PathBuf> {
    anyhow::ensure!(
        is_copy(root, dir),
        "pas une copie en ligne : {}",
        dir.display()
    );
    let name = dir
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let mut to = saves.join(format!("en_ligne_{name}"));
    let mut n = 2;
    while to.exists() {
        to = saves.join(format!("en_ligne_{name}_{n}"));
        n += 1;
    }
    fs::rename(dir, &to).with_context(|| format!("déplacement vers {}", to.display()))?;
    let level = to.join("level.txt");
    if let Ok(text) = fs::read_to_string(&level) {
        let text: String = text
            .lines()
            .map(|l| match l.strip_prefix("name=") {
                Some(old) => format!("name={} ({why})\n", old.trim()),
                None => format!("{l}\n"),
            })
            .collect();
        fs::write(&level, text)?;
    }
    Ok(to)
}

/// Avant la v325, un monde en ligne se jouait dans le monde solo dont il venait (chez son
/// créateur), ou dans sa carte téléchargée (chez un invité devenu propriétaire) : sans copie
/// retenue, son dernier état est là. Le monde solo ou la carte téléchargée nommés `map`, le
/// plus récemment joué ; chez son créateur, le monde solo d'abord.
pub fn before_v325(root: &Path, saves: &Path, map: &str, creator: bool) -> Option<PathBuf> {
    let best = |dir: &Path| {
        crate::save::list_worlds(dir)
            .into_iter()
            .filter(|w| !w.minecraft && w.meta.name == map)
            .max_by_key(|w| w.meta.last_played)
            .map(|w| w.dir)
    };
    let (solo, maps) = (best(saves), best(&root.join("maps")));
    if creator {
        solo.or(maps)
    } else {
        maps.or(solo)
    }
}

/// Au lancement : les lignes dont la copie n'existe plus, les copies à moitié faites, et
/// celles qu'aucun monde ne retient, où l'on n'a pas joué, vieilles de plus d'un jour.
pub fn tidy(root: &Path) {
    let base = root.join(DIR);
    let Ok(entries) = fs::read_dir(&base) else {
        return;
    };
    let mut lines = read(root);
    let n = lines.len();
    lines.retain(|l| abs(root, &l.dir).join("level.txt").exists());
    if lines.len() != n {
        write(root, &lines);
    }
    let now = crate::save::now();
    for e in entries.flatten() {
        let p = e.path();
        if !p.is_dir() || lines.iter().any(|l| abs(root, &l.dir) == p) {
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        let stamp: Option<u64> = name.split('-').next().and_then(|s| s.parse().ok());
        let saved = fs::metadata(p.join("level.txt"))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs());
        let orphan = match (stamp, saved) {
            (Some(s), Some(t)) => t <= s + UNTOUCHED_S && now >= s + ORPHAN_S,
            // (Pas un monde : rien d'utile dedans.)
            (Some(s), None) => now >= s + ORPHAN_S,
            _ => false,
        };
        let half = name.starts_with('.')
            && name[1..]
                .split('-')
                .next()
                .and_then(|s| s.parse::<u64>().ok())
                .is_some_and(|s| now >= s + ORPHAN_S);
        if orphan || half {
            log::info!("copie en ligne sans monde, supprimée : {}", p.display());
            let _ = fs::remove_dir_all(&p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        let r = std::env::temp_dir().join(format!("minelib-en-ligne-{}", rand::random::<u32>()));
        fs::create_dir_all(r.join("saves")).unwrap();
        r
    }

    fn files(dir: &Path) -> Vec<(String, Vec<u8>)> {
        let mut v: Vec<(String, Vec<u8>)> = fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter(|e| e.path().is_file())
            .map(|e| {
                (
                    e.file_name().to_string_lossy().into_owned(),
                    fs::read(e.path()).unwrap(),
                )
            })
            .collect();
        v.sort();
        v
    }

    /// Ouvrir un monde solo en ligne en fait une copie à part : la copie change, le monde
    /// solo non ; l'ouvrir de nouveau en fait une autre (un autre serveur). Le registre
    /// retrouve la copie d'un monde en ligne, une seule par monde.
    #[test]
    fn an_online_world_lives_in_its_own_copy_never_in_the_solo_world() {
        let root = root();
        let saves = root.join("saves");
        let solo = crate::save::create_world(&saves, "Île", 7).unwrap();
        fs::write(solo.join("edits.bin"), [1, 2, 3]).unwrap();
        let before = files(&solo);
        let a = fork(&root, &solo).unwrap();
        assert!(is_copy(&root, &a) && !is_copy(&root, &solo));
        assert_eq!(files(&a), before);
        fs::write(a.join("edits.bin"), [9, 9]).unwrap();
        assert_eq!(files(&solo), before, "le monde solo ne bouge pas");
        let b = fork(&root, &solo).unwrap();
        assert_ne!(a, b);
        assert_eq!(
            files(&b),
            before,
            "rouvert en ligne : une copie neuve du monde solo"
        );
        keep(&root, "r1", "ana/Île", &a);
        keep(&root, "r1", "ana/Île#2", &b);
        assert_eq!(find(&root, "r1", "ana/Île"), Some(a.clone()));
        assert_eq!(find(&root, "r1", "ana/Île#2"), Some(b.clone()));
        // (Un autre relais qui ne le connaît pas : la copie du même monde quand même.)
        assert_eq!(find(&root, "r2", "ana/Île"), Some(a.clone()));
        assert_eq!(find(&root, "r1", "ana/autre"), None);
        // Une nouvelle copie du même monde (un réveil depuis une carte téléchargée) : celle
        // d'avant est supprimée ; une carte téléchargée retenue puis remplacée reste.
        let c = fork(&root, &a).unwrap();
        keep(&root, "r1", "ana/Île", &c);
        assert_eq!(find(&root, "r1", "ana/Île"), Some(c.clone()));
        assert!(!a.exists());
        let maps = root.join("maps").join("0123456789ab");
        fs::create_dir_all(&maps).unwrap();
        fs::write(maps.join("level.txt"), "name=Île\n").unwrap();
        keep(&root, "r1", "ana/Île", &maps);
        assert!(!c.exists() && maps.exists());
        let d = fork(&root, &maps).unwrap();
        keep(&root, "r1", "ana/Île", &d);
        assert!(maps.exists(), "la carte téléchargée reste");
        // Monde supprimé par son créateur : sa copie aussi ; le monde solo, jamais.
        forget(&root, "r1", "ana/Île");
        assert!(!d.exists() && find(&root, "r1", "ana/Île").is_none());
        assert_eq!(files(&solo), before);
        assert_eq!(find(&root, "r1", "ana/Île#2"), Some(b));
        let _ = fs::remove_dir_all(&root);
    }

    /// (v349) « Ceux qui y ont joué l'ont toujours et peuvent le relancer » : un monde oublié
    /// par le relais pour faire de la place devient, chez qui en garde la copie, un monde solo
    /// complet (la clé de son terrain, sa graine chez le créateur), avec ce qu'on y avait
    /// construit. Un monde d'un autre relais, ou dont ce PC n'a pas de copie, ne bouge pas.
    #[test]
    fn a_world_the_relay_forgot_becomes_a_solo_world_to_reopen() {
        let root = root();
        let saves = root.join("saves");
        let solo = crate::save::create_world(&saves, "Île", 7).unwrap();
        let copy = fork(&root, &solo).unwrap();
        // Comme la copie d'un serveur de construction : sans graine ni clé.
        let level = fs::read_to_string(copy.join("level.txt")).unwrap();
        let bare: String = level
            .lines()
            .filter(|l| !l.starts_with("graine=") && !l.starts_with("cle="))
            .map(|l| {
                if l.starts_with("seed=") {
                    "seed=0\n".to_string()
                } else {
                    format!("{l}\n")
                }
            })
            .collect();
        fs::write(copy.join("level.txt"), bare).unwrap();
        fs::write(copy.join("edits.bin"), [4, 5, 6]).unwrap();
        keep(&root, "r1", "ana/Île", &copy);
        let key = minelib_net::graine::terrain_key(7);
        let w = |world: &str| minelib_net::proto::ForgottenWorld {
            world: world.into(),
            name: "Île".into(),
            idle_days: 64,
            terrain: key,
            seed: 7,
        };
        assert!(
            forgotten(&root, &saves, "r2", &w("ana/Île")).is_none(),
            "autre relais"
        );
        assert!(
            forgotten(&root, &saves, "r1", &w("ana/autre")).is_none(),
            "pas de copie"
        );
        assert!(copy.exists());
        let to = forgotten(&root, &saves, "r1", &w("ana/Île")).unwrap();
        assert!(!copy.exists() && to.starts_with(&saves));
        assert_eq!(fs::read(to.join("edits.bin")).unwrap(), [4, 5, 6]);
        let meta = crate::save::list_worlds(&saves)
            .into_iter()
            .find(|m| m.dir == to)
            .unwrap()
            .meta;
        assert_eq!(meta.name, "Île (oublié par le relais)");
        assert_eq!((meta.key, meta.graine), (Some(key), Some(7)));
        assert_eq!(meta.seed, minelib_net::graine::public_seed(key));
        assert!(find(&root, "r1", "ana/Île").is_none());
        // Le monde solo d'origine ne bouge pas.
        assert!(solo.join("level.txt").exists());
        let _ = fs::remove_dir_all(&root);
    }

    /// Au lancement : une copie sans monde, où l'on n'a pas joué, part au bout d'un jour ; une
    /// copie où l'on a joué reste (elle devient un monde solo quand on la quitte) ; une copie
    /// retenue reste toujours.
    #[test]
    fn tidying_removes_only_untouched_copies_no_world_keeps() {
        let root = root();
        let base = root.join(DIR);
        let old = crate::save::now() - ORPHAN_S - 10;
        let mk = |name: &str| {
            let d = base.join(name);
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("level.txt"), "name=X\n").unwrap();
            d
        };
        let fresh = mk(&crate::save::now().to_string());
        let untouched = mk(&old.to_string());
        let played = mk(&format!("{}-1", old - 1000));
        let kept = mk(&format!("{}-2", old - 1000));
        let half = base.join(format!(".{old}"));
        fs::create_dir_all(&half).unwrap();
        // (Des fichiers récents : on y a joué bien après leur copie.)
        keep(&root, "r", "ana/x", &kept);
        let set = |d: &Path, secs: u64| {
            let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
            fs::File::options()
                .write(true)
                .open(d.join("level.txt"))
                .unwrap()
                .set_modified(t)
                .unwrap();
        };
        set(&untouched, old + 5);
        tidy(&root);
        assert!(fresh.exists(), "peut-être en train d'être ouverte");
        assert!(!untouched.exists() && !half.exists());
        assert!(played.exists() && kept.exists());
        assert_eq!(find(&root, "r", "ana/x"), Some(kept));
        // Une copie où l'on a joué sans monde en ligne : un monde solo, sous un nom qui le dit.
        let solo = to_solo(&root, &root.join("saves"), &played).unwrap();
        let w = crate::save::list_worlds(&root.join("saves"));
        assert_eq!(w.len(), 1);
        assert_eq!(
            (w[0].dir.clone(), w[0].meta.name.as_str()),
            (solo, "X (pas ouvert en ligne)")
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// Sans copie retenue (un monde endormi avant la v325) : chez son créateur, le monde solo
    /// de ce nom d'abord ; chez un invité devenu propriétaire, sa carte téléchargée.
    #[test]
    fn a_world_from_before_v325_is_found_where_it_was_played() {
        let root = root();
        let saves = root.join("saves");
        let solo = crate::save::create_world(&saves, "Île", 7).unwrap();
        let maps = root.join("maps").join("0123456789ab");
        fs::create_dir_all(&maps).unwrap();
        fs::write(maps.join("level.txt"), "name=Île\nlast_played=5\n").unwrap();
        assert_eq!(before_v325(&root, &saves, "Île", true), Some(solo.clone()));
        assert_eq!(before_v325(&root, &saves, "Île", false), Some(maps.clone()));
        fs::remove_dir_all(&maps).unwrap();
        assert_eq!(before_v325(&root, &saves, "Île", false), Some(solo));
        assert_eq!(before_v325(&root, &saves, "Autre", true), None);
        let _ = fs::remove_dir_all(&root);
    }
}
