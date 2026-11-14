//! Réseau de Minelib : protocole entre le jeu et le relais, chiffrement des échanges et
//! des comptes.
//!
//! Un compte n'est qu'un pseudo et une paire de clés Ed25519 : la clé privée est chiffrée
//! par une clé tirée du mot de passe (Argon2id) et gardée par le relais, qui ne voit jamais
//! ni le mot de passe ni la clé en clair. Se connecter, c'est récupérer ce coffre, l'ouvrir
//! sur sa machine et signer un défi du relais. Aucun e-mail, aucun numéro.

pub mod client;
pub mod crypto;
pub mod game;
pub mod hw;
pub mod lan;
pub mod proto;
pub mod totp;
pub mod transport;
pub mod voice;

/// Port par défaut du relais.
pub const DEFAULT_PORT: u16 = 27650;
