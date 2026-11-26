//! (v352) Une carte prise par le jeu aux machines qui l'ont, par iroh-blobs : ici le nœud
//! du relais, et non plus morceau par morceau par la connexion au relais (`MapGet`, qui
//! reste le dernier recours). Le jeu la garde ensuite, pour la servir à son tour.
//!
//! (Un fichier d'essai à lui : `minelib_net::cartes` vaut pour tout le programme.)

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use minelib_net::client::{Client, Cmd, Event};
use minelib_p2p::cartes::Limits;

fn wait_for(c: &Client, what: &str, mut pred: impl FnMut(&Event) -> bool) -> Event {
    let t0 = Instant::now();
    loop {
        while let Some(e) = c.poll() {
            if std::env::var("CARTES_DEBUG").is_ok() {
                eprintln!("({what}) {:?}", e);
            }
            if let Event::Error(m) = &e {
                eprintln!("({what}) erreur : {m}");
            }
            if pred(&e) {
                return e;
            }
        }
        assert!(
            t0.elapsed() < Duration::from_secs(60),
            "{what} : rien après 60 s"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn log_in(addr: &str, key: [u8; 32], pseudo: &str) -> Client {
    let c = Client::spawn(addr.to_string(), Some(key));
    c.send(Cmd::Register {
        pseudo: pseudo.into(),
        password: "mot de passe assez long".into(),
    });
    wait_for(&c, pseudo, |e| matches!(e, Event::LoggedIn { .. }));
    c
}

#[test]
fn a_game_takes_a_map_from_the_machines_that_have_it_and_keeps_it() {
    minelib_net::client::GAME_VERSION.store(352, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("minelib-cartes-jeu-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let h = minelib_relay::start(minelib_relay::Config {
        dir: dir.join("relais"),
        bind: "127.0.0.1:0".into(),
        pow_bits: 4,
        name: "essai".into(),
    })
    .unwrap();
    let addr = h.addr.to_string();
    let net = minelib_p2p::LocalNet::new();
    let relay_node = minelib_p2p::Node::start_local(&net).unwrap();
    let relay_maps = relay_node
        .share_maps(&dir.join("cartes_relais"), Limits::SERVER)
        .unwrap();
    h.share_maps(relay_maps.clone());
    // Une carte FPS envoyée au relais par son auteur (dont le jeu ne garde pas de cartes).
    let bytes = minelib_net::game::MapPackage {
        seed: 7,
        generator: "procedural2".into(),
        edits: (0..700_000u32).map(|i| (i * 7 % 251) as u8).collect(),
        chunks: Vec::new(),
        torches: Vec::new(),
        fps: Default::default(),
    }
    .to_bytes();
    let sig = minelib_net::game::signature(&bytes);
    minelib_net::cartes::set(None);
    let author = log_in(&addr, h.public, "Auteur");
    author.send(Cmd::UploadMap {
        sig,
        name: "Arène".into(),
        bytes: bytes.clone(),
        fps: true,
    });
    wait_for(
        &author,
        "envoi",
        |e| matches!(e, Event::MapUploaded(s) if *s == sig),
    );
    let t0 = Instant::now();
    while relay_maps.held_map(&sig).is_none() {
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "pas servie par le nœud du relais"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // Un joueur dont le jeu garde et sert des cartes.
    let game_node = minelib_p2p::Node::start_local(&net).unwrap();
    let game_maps = game_node
        .share_maps(&dir.join("cartes_jeu"), Limits::GAME)
        .unwrap();
    minelib_net::cartes::set(Some(game_maps.clone()));
    let player = log_in(&addr, h.public, "Joueur");
    wait_for(
        &player,
        "niveau du relais",
        |e| matches!(e, Event::RelayLevel(l) if *l == minelib_net::proto::RELAY_LEVEL),
    );
    let served_before = relay_maps.served();
    player.send(Cmd::DownloadMap(sig));
    let got = match wait_for(&player, "carte", |e| {
        matches!(e, Event::MapDownloaded { .. } | Event::MapFailed(..))
    }) {
        Event::MapDownloaded { sig: s, bytes } if s == sig => bytes,
        e => panic!("{e:?}"),
    };
    assert!(got == bytes, "pas la même carte");
    assert!(
        relay_maps.served() >= served_before + bytes.len() as u64 / 2,
        "elle n'est pas venue du nœud du relais"
    );
    assert!(
        game_maps.held_map(&sig).is_some(),
        "le jeu la garde pour la servir"
    );

    // Un relais dont le nœud ne répond pas aux cartes (ici, introuvable) : la carte vient
    // quand même, du relais seul, comme avant.
    let h2 = minelib_relay::start(minelib_relay::Config {
        dir: dir.join("relais2"),
        bind: "127.0.0.1:0".into(),
        pow_bits: 4,
        name: "essai 2".into(),
    })
    .unwrap();
    let elsewhere = minelib_p2p::LocalNet::new();
    let far_node = minelib_p2p::Node::start_local(&elsewhere).unwrap();
    let far_maps = far_node
        .share_maps(&dir.join("cartes_relais2"), Limits::SERVER)
        .unwrap();
    h2.share_maps(far_maps.clone());
    let bytes2 = minelib_net::game::MapPackage {
        seed: 8,
        generator: "procedural2".into(),
        edits: vec![5; 90_000],
        chunks: Vec::new(),
        torches: Vec::new(),
        fps: Default::default(),
    }
    .to_bytes();
    let sig2 = minelib_net::game::signature(&bytes2);
    minelib_net::cartes::set(None);
    let author2 = log_in(&h2.addr.to_string(), h2.public, "Auteur");
    author2.send(Cmd::UploadMap {
        sig: sig2,
        name: "Ailleurs".into(),
        bytes: bytes2.clone(),
        fps: true,
    });
    wait_for(
        &author2,
        "envoi 2",
        |e| matches!(e, Event::MapUploaded(s) if *s == sig2),
    );
    let t0 = Instant::now();
    while far_maps.held_map(&sig2).is_none() {
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "pas servie par le nœud du relais 2"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    minelib_net::cartes::set(Some(game_maps.clone()));
    let player2 = log_in(&h2.addr.to_string(), h2.public, "Joueur");
    wait_for(&player2, "niveau du relais 2", |e| {
        matches!(e, Event::RelayLevel(_))
    });
    let t0 = Instant::now();
    player2.send(Cmd::DownloadMap(sig2));
    let got2 = match wait_for(&player2, "carte 2", |e| {
        matches!(e, Event::MapDownloaded { .. } | Event::MapFailed(..))
    }) {
        Event::MapDownloaded { sig: s, bytes } if s == sig2 => bytes,
        e => panic!("{e:?}"),
    };
    assert!(got2 == bytes2);
    assert_eq!(far_maps.served(), 0);
    assert!(t0.elapsed() < Duration::from_secs(30));
    assert!(
        game_maps.held_map(&sig2).is_some(),
        "venue du relais seul, elle est gardée aussi"
    );

    // Par le tunnel iroh, le relais sait quel nœud sert les cartes d'un joueur : un autre
    // jeu prend la carte chez lui.
    let h3 = minelib_relay::start(minelib_relay::Config {
        dir: dir.join("relais3"),
        bind: "127.0.0.1:0".into(),
        pow_bits: 4,
        name: "essai 3".into(),
    })
    .unwrap();
    let relay3 = minelib_p2p::Node::start_local(&net).unwrap();
    relay3.serve(h3.addr);
    h3.set_own_node(relay3.id());
    let local = game_node
        .tunnel_checked(relay3.id(), Duration::from_secs(10))
        .unwrap();
    let holder = log_in(&local.to_string(), h3.public, "Joueur");
    wait_for(&holder, "niveau du relais 3", |e| {
        matches!(e, Event::RelayLevel(_))
    });
    let other_node = minelib_p2p::Node::start_local(&net).unwrap();
    let other_maps = other_node
        .share_maps(&dir.join("cartes_autre"), Limits::GAME)
        .unwrap();
    minelib_net::cartes::set(Some(other_maps.clone()));
    let other = log_in(&h3.addr.to_string(), h3.public, "Autre");
    wait_for(&other, "niveau du relais 3 (autre)", |e| {
        matches!(e, Event::RelayLevel(_))
    });
    let served = game_maps.served();
    other.send(Cmd::DownloadMap(sig));
    let got3 = match wait_for(&other, "carte 3", |e| {
        matches!(e, Event::MapDownloaded { .. } | Event::MapFailed(..))
    }) {
        Event::MapDownloaded { sig: s, bytes } if s == sig => bytes,
        e => panic!("{e:?}"),
    };
    assert!(got3 == bytes);
    assert!(
        game_maps.served() >= served + bytes.len() as u64 / 2,
        "elle n'est pas venue du jeu qui l'avait"
    );
    drop(holder);
    minelib_net::cartes::set(None);
    let _ = std::fs::remove_dir_all(&dir);
}
