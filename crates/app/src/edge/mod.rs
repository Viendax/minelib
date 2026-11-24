//! (v261) Le navigateur des écrans : **Edge (WebView2)**, dans ce même programme
//! (`minelib.exe --navigateur-edge …`, lancé à part par le jeu).
//!
//! Choix du propriétaire (24/11/2026) : « Edge pour tout ». Le Chromium de l'ancien
//! navigateur (`crates/browser`, CEF) n'avait pas les codecs H.264 et AAC (brevets) : ni
//! Twitch, ni Vimeo, ni Dailymotion. Edge les a, sous licence de Microsoft, et il est déjà
//! dans Windows 10 et 11 : plus rien à télécharger pour les amis, et le navigateur est
//! toujours à la version du jeu (c'est le même programme).
//!
//! Le protocole avec le jeu est celui de l'ancien navigateur (`proto`), les scripts des pages
//! aussi (`scripts`, adaptés), comme le filtre (`filtre`, le même fichier) et le volume
//! (`volume`).

#[path = "../../../browser/src/filtre.rs"]
#[allow(dead_code)]
pub mod filtre;
pub mod proto;
pub mod scripts;
#[path = "../../../browser/src/volume.rs"]
#[allow(dead_code)]
mod volume;
#[cfg(windows)]
mod win;

/// L'argument qui fait de ce programme le navigateur des écrans.
pub const ARG: &str = "--navigateur-edge";

/// Code de sortie : Edge WebView2 n'est pas installé.
pub const SANS_WEBVIEW2: i32 = 3;

/// Le navigateur : rend son code de sortie.
pub fn main(args: Vec<String>) -> i32 {
    let opts = proto::Options::parse(args.into_iter());
    #[cfg(windows)]
    {
        win::main(opts)
    }
    #[cfg(not(windows))]
    {
        let _ = opts;
        eprintln!("navigateur Edge : seulement sous Windows");
        1
    }
}
