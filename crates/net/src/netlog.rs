//! (v307) Journal réseau partagé.
//!
//! Le jeu écrit déjà dans son journal, à intervalles réguliers, des lignes sur l'état de
//! son réseau (« réseau (construction…) », « réseau (FPS…) », « tunnel : … », « passeur
//! (…) »). Elles ne contiennent ni pseudo, ni adresse, ni numéro de nœud. Un joueur qui
//! l'accepte (question posée une seule fois, au menu d'accueil ; modifiable dans les
//! paramètres avancés) les envoie au relais auquel il est connecté, si l'hôte de ce relais
//! a demandé à les recevoir (paramètres avancés, ou `--journaux` pour le relais seul).
//! Le relais les range dans `journaux_reseau.txt`, avec ses propres relevés et ses alertes
//! (voir `minelib_relay`), sous un numéro tiré au hasard par connexion.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

static SHARE: AtomicBool = AtomicBool::new(false);
static QUEUE: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

/// Plus longue ligne gardée, et lignes en attente au plus (les plus anciennes sont jetées).
pub const LINE_MAX: usize = 400;
const KEPT: usize = 200;
/// Débuts des lignes du journal du jeu qui partent (rien d'autre ne part jamais).
const SHARED: [&str; 3] = ["réseau (", "tunnel : ", "passeur ("];
/// Débuts des lignes que le relais accepte d'un jeu.
const ACCEPTED: [&str; 3] = SHARED;

/// Le joueur partage (ou non) son journal réseau.
pub fn set_share(on: bool) {
    SHARE.store(on, Ordering::Relaxed);
    if !on {
        QUEUE.lock().unwrap().clear();
    }
}

pub fn sharing() -> bool {
    SHARE.load(Ordering::Relaxed)
}

/// Ce que vaut une ligne : rien si elle n'est pas une ligne réseau connue ; sinon elle-même,
/// sur une ligne, sans caractère de commande, coupée à `LINE_MAX`.
pub fn clean(text: &str) -> Option<String> {
    let t = text.trim();
    if !ACCEPTED.iter().any(|p| t.starts_with(p)) {
        return None;
    }
    Some(
        t.chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .take(LINE_MAX)
            .collect(),
    )
}

/// Une ligne que le jeu vient d'écrire dans son journal (sans l'en-tête de date) : gardée
/// pour le relais si le joueur partage et si c'est une ligne réseau.
pub fn offer(text: &str) {
    if !sharing() || !SHARED.iter().any(|p| text.trim_start().starts_with(p)) {
        return;
    }
    if let Some(line) = clean(text) {
        let mut q = QUEUE.lock().unwrap();
        q.push_back(line);
        while q.len() > KEPT {
            q.pop_front();
        }
    }
}

/// Jusqu'à `max` lignes en attente (elles ne partent qu'une fois).
pub fn take(max: usize) -> Vec<String> {
    let mut q = QUEUE.lock().unwrap();
    let n = q.len().min(max);
    q.drain(..n).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Seules les lignes réseau connues partent, et seulement si le joueur partage ; une
    /// ligne trop longue ou sur plusieurs lignes est remise d'aplomb.
    #[test]
    fn only_known_network_lines_are_shared_and_only_when_agreed() {
        set_share(false);
        offer("réseau (FPS, 30 s) : rien");
        assert!(take(10).is_empty());
        set_share(true);
        offer("réseau (FPS, 30 s) : 3 images en retard");
        offer("  tunnel : ouvert, avec une route par des passeurs");
        offer("compte : Viendax connecté depuis 82.1.2.3");
        offer("relais (santé, 60 s) : 4 connexions");
        let got = take(10);
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got[1].starts_with("tunnel : "));
        assert!(take(10).is_empty());
        assert_eq!(clean("mot de passe : x"), None);
        let long = format!("passeur (60 s) : {}\nsuite", "x".repeat(900));
        let c = clean(&long).unwrap();
        assert!(c.chars().count() == LINE_MAX && !c.contains('\n'));
        set_share(false);
    }
}
