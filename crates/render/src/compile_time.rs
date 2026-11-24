//! (v295) Durées des compilations de shaders lourds (monde, lumières), gardées d'un lancement
//! à l'autre (`durees_shaders.txt`, dans le dossier des données du jeu) : l'écran de
//! chargement d'un monde s'en sert pour que sa barre avance au rythme de l'attente réelle.
//!
//! Viendax, 24 novembre, 21 h 03 : « la barre de chargement quand on rejoint un monde, le
//! temps ne correspond pas au temps qu'on attend ». Elle suivait les chunks affichés : pleine
//! en une vingtaine de secondes, puis on attendait encore la fin de la compilation des shaders
//! du monde, lancée au démarrage du jeu (66 s chez lui sous DirectX ; son journal : monde
//! ouvert à 20 h 02 min 35, chunks tous là vers 20 h 03, entrée à 20 h 03 min 24, à la fin de
//! « shaders du monde (TAA) compilés en 66.2 s »).

use std::path::Path;
use std::sync::Mutex;

/// Fichier des durées (dossier des données du jeu).
pub const FILE: &str = "durees_shaders.txt";
/// Shaders lourds du monde (`taa::build_heavy`).
pub const WORLD: &str = "monde";
/// Shaders des lumières de l'éditeur (`Renderer::spawn_lit`).
pub const LIGHTS: &str = "lumieres";
/// Mesures gardées par compilation : leur moyenne est la durée attendue.
const KEEP: usize = 5;
/// Sans mesure d'un lancement précédent : à peu près ce que prennent ces compilations au
/// lancement chez Viendax (RTX 4050, DirectX 12 : 54 à 96 s pour le monde, 30 à 64 s pour les
/// lumières le 24 novembre).
pub const DEFAULT_S: f32 = 60.0;

/// Une écriture à la fois (les deux compilations finissent sur des fils à part).
static WRITING: Mutex<()> = Mutex::new(());

/// Durées gardées pour `name` dans le texte du fichier (lignes « nom = 66.2 67.5 … »).
fn durations(text: &str, name: &str) -> Vec<f32> {
    text.lines()
        .filter_map(|l| l.split_once('='))
        .filter(|(k, _)| k.trim() == name)
        .flat_map(|(_, v)| v.split_whitespace())
        .filter_map(|v| v.parse::<f32>().ok())
        .filter(|v| v.is_finite() && *v > 0.0 && *v < 3600.0)
        .collect()
}

/// Durée attendue d'après le texte du fichier : moyenne des dernières mesures de `name`,
/// sinon `DEFAULT_S`.
fn expected_in(text: &str, name: &str) -> f32 {
    let d = durations(text, name);
    if d.is_empty() {
        DEFAULT_S
    } else {
        d.iter().sum::<f32>() / d.len() as f32
    }
}

/// Texte du fichier avec `secs` ajouté aux mesures de `name` (seules les `KEEP` dernières
/// restent ; les autres lignes ne changent pas).
fn with_record(text: &str, name: &str, secs: f32) -> String {
    let mut d = durations(text, name);
    d.push(secs);
    let skip = d.len().saturating_sub(KEEP);
    let kept: Vec<String> = d[skip..].iter().map(|v| format!("{v:.1}")).collect();
    let mut out: Vec<String> = text
        .lines()
        .filter(|l| l.split_once('=').is_none_or(|(k, _)| k.trim() != name))
        .map(str::to_string)
        .collect();
    out.push(format!("{name} = {}", kept.join(" ")));
    out.join("\n") + "\n"
}

/// Durée attendue (s) de la compilation `name` dans le dossier `dir`.
pub fn expected_at(dir: &Path, name: &str) -> f32 {
    expected_in(&std::fs::read_to_string(dir.join(FILE)).unwrap_or_default(), name)
}

/// Durée attendue (s) de la compilation `name` (dossier des données du jeu).
pub fn expected(name: &str) -> f32 {
    expected_at(&minelib_assets::data_dir(), name)
}

/// Garde la durée d'une compilation qui vient de finir, dans le dossier `dir`.
pub fn record_at(dir: &Path, name: &str, secs: f32) {
    if !secs.is_finite() || secs <= 0.0 {
        return;
    }
    let _guard = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let path = dir.join(FILE);
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    if let Err(e) = std::fs::write(&path, with_record(&text, name, secs)) {
        log::warn!("durée de compilation des shaders non gardée : {e}");
    }
}

/// Garde la durée d'une compilation qui vient de finir (dossier des données du jeu ; pas
/// pendant les essais).
pub fn record(name: &str, secs: f32) {
    if cfg!(test) {
        return;
    }
    record_at(&minelib_assets::data_dir(), name, secs);
}

/// (v295) Secondes qui restent, estimées, à une compilation lancée il y a `elapsed` s et qui
/// prend d'habitude `expected` s. Plus longue que d'habitude : on ne sait plus, il reste
/// « un peu » (la barre ralentit au bout au lieu de se remplir avant la fin).
pub fn left(elapsed: f32, expected: f32) -> f32 {
    (expected - elapsed).max(0.0) + 0.5
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_durations_are_kept_from_one_launch_to_the_next() {
        let dir = std::env::temp_dir().join(format!("minelib_durees_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(expected_at(&dir, WORLD), DEFAULT_S, "rien de mesuré : la valeur par défaut");
        for s in [96.0, 54.5, 73.3, 66.8, 67.5, 66.2] {
            record_at(&dir, WORLD, s);
        }
        record_at(&dir, LIGHTS, 57.4);
        let text = std::fs::read_to_string(dir.join(FILE)).unwrap();
        assert_eq!(durations(&text, WORLD), vec![54.5, 73.3, 66.8, 67.5, 66.2], "les 5 dernières");
        let avg = (54.5 + 73.3 + 66.8 + 67.5 + 66.2) / 5.0;
        assert!((expected_at(&dir, WORLD) - avg).abs() < 1e-3);
        assert_eq!(expected_at(&dir, LIGHTS), 57.4, "chaque compilation a les siennes");
        // Un fichier abîmé ne gêne pas.
        std::fs::write(dir.join(FILE), "monde = abc -3 70\nn'importe quoi\n").unwrap();
        assert_eq!(expected_at(&dir, WORLD), 70.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_compile_slower_than_usual_never_looks_finished() {
        assert_eq!(left(20.0, 66.0), 46.5);
        assert!(left(80.0, 66.0) > 0.0, "plus longue que d'habitude : il reste encore un peu");
    }
}
