//! (v352) Cartes partagées : ce que le fil du relais (`client`) demande au nœud iroh du
//! programme, qui garde et sert les cartes (`minelib_p2p::cartes`, hors de ce crate).
//!
//! Le jeu (ou le passeur sans joueur) le donne au démarrage (`set`). Sans lui, tout se
//! passe comme avant : les cartes viennent du relais seul (`Request::MapGet`).

use std::sync::{Arc, Mutex};

use crate::proto::{MapHeld, MapSource};

pub trait Peers: Send + Sync {
    /// Identifiant du nœud iroh qui sert ces cartes.
    fn node(&self) -> [u8; 32];
    /// Les cartes gardées ici.
    fn held(&self) -> Vec<MapHeld>;
    /// Les cartes sorties depuis la dernière fois (place), à dire au relais.
    fn gone(&self) -> Vec<[u8; 32]>;
    /// Place encore libre (octets).
    fn room(&self) -> u64;
    /// Garde une carte qu'on a entière (reçue du relais, ou faite et envoyée par ce jeu).
    fn keep(&self, sig: [u8; 32], bytes: &[u8]);
    /// Prend une carte aux machines qui l'ont, et la garde ici si sa signature est bonne.
    fn fetch(&self, source: &MapSource, progress: &mut dyn FnMut(f32)) -> anyhow::Result<Vec<u8>>;
}

static PEERS: Mutex<Option<Arc<dyn Peers>>> = Mutex::new(None);

/// Le nœud de ce programme qui garde et sert les cartes (`None` : plus aucun).
pub fn set(peers: Option<Arc<dyn Peers>>) {
    *PEERS.lock().unwrap() = peers;
}

pub fn get() -> Option<Arc<dyn Peers>> {
    PEERS.lock().unwrap().clone()
}
