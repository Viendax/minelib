//! `minelib-relay` : relais Minelib à lancer sur un serveur (ou un PC) joignable par les
//! joueurs.
//!
//! Options : `--port <n>` (27650), `--dir <dossier>` (`relay_data`), `--pow <bits>` (20),
//! `--name <nom>`, `--no-dht` (ne pas s'inscrire dans le DHT).
//!
//! Par défaut, le relais est joignable sans port ouvert (iroh) et s'inscrit dans le DHT de
//! BitTorrent : les jeux réglés sur `relay=auto` le trouvent tout seuls.
//!
//! (v320) `--passeur` : pas de relais, un passeur sans joueur (`passeur_seul`) pour une
//! machine toujours allumée. Options : `--dir <dossier>` (`passeur_data`), `--ip <adresse
//! publique>` (sinon celle que voient les serveurs d'iroh), `--port <premier port UDP>` (un
//! par relais servi ; par défaut n'importe lesquels), `--relais "<nœud> <clé> <nom>"` (en
//! plus des relais officiels et de ceux du DHT ; plusieurs fois possible).
//!
//! (v345) `--invitation "<secret> <nœud> <clé> <nom>"` (passeur) : l'invitation donnée sur
//! ce relais (`/forwarder invite` dans le jeu ; v346 : le seul compte de
//! `minelib_net::proto::can_invite_forwarders`). Le passeur la lui donne à chaque connexion,
//! et y devient de confiance : il porte aussi les parties FPS.
//! `--admin <pseudo>` (relais) : ce compte administre le relais (`admins.txt`, plusieurs fois
//! possible) : bannir pour triche.
//!
//! (v349) `--place-mondes <Mo>` (relais, 1 000 par défaut) : la place des mondes (registre,
//! réglages, sacs, et (v350) copies des mondes endormis avec leur carte). Au-delà, les copies
//! des mondes endormis depuis au moins 30 jours partent d'abord, le moins récemment actif
//! d'abord, puis ces mondes eux-mêmes ; leur créateur et leur gardien l'apprennent à leur
//! prochaine connexion et gardent leur copie, en monde solo.
//!
//! (v347) Un passeur envoie ses lignes de journal réseau aux relais qui les demandent
//! (`minelib_net::netlog` : ni pseudo, ni adresse, ni nœud), comme un jeu dont le joueur l'a
//! accepté, sans question : il n'y a pas de joueur. `--sans-journaux` : ne pas les envoyer.

use anyhow::Result;

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(
        "info,mainline=error,iroh=warn,iroh_relay=warn,iroh_quinn=warn,iroh_quinn_proto=warn,\
         quinn=warn,quinn_proto=warn,tracing=warn,portmapper=warn,netwatch=warn,pkarr=warn",
    ))
    // (v347) Chaque ligne est aussi proposée au journal réseau partagé : seules les lignes
    // réseau connues y entrent, et seulement s'il est partagé (passeur).
    .format(|buf, record| {
        use std::io::Write;
        let msg = record.args().to_string();
        minelib_net::netlog::offer(&msg);
        writeln!(
            buf,
            "[{} {:<5} {}] {msg}",
            buf.timestamp(),
            record.level(),
            record.target()
        )
    })
    .init();
    // (v320) Passeur sans joueur.
    if std::env::args().any(|a| a == "--passeur") {
        return passeur_main();
    }
    // (v307) `--journaux` : recevoir les journaux réseau des joueurs qui les partagent, le
    // relevé de santé et les alertes du relais, dans `journaux_reseau.txt`.
    if std::env::args().any(|a| a == "--journaux") {
        minelib_relay::set_receive_logs(true);
    }
    let mut cfg = minelib_relay::Config {
        dir: "relay_data".into(),
        bind: format!("0.0.0.0:{}", minelib_net::DEFAULT_PORT),
        pow_bits: 20,
        name: "Minelib".into(),
    };
    let mut args: Vec<String> = std::env::args().collect();
    let dht = !args.iter().any(|a| a == "--no-dht");
    args.retain(|a| a != "--no-dht");
    let mut admins: Vec<String> = Vec::new();
    let mut i = 1;
    while i < args.len() {
        let v = args.get(i + 1).cloned().unwrap_or_default();
        match args[i].as_str() {
            "--port" => cfg.bind = format!("0.0.0.0:{v}"),
            "--dir" => cfg.dir = v.into(),
            "--pow" => cfg.pow_bits = v.parse().unwrap_or(20),
            "--name" => cfg.name = v,
            // (v345) Administrateur du relais (`admins.txt`).
            "--admin" => admins.push(v),
            // (v349) Place des mondes, en Mo (1 000 par défaut) : au-delà, des mondes endormis
            // depuis au moins 30 jours sont oubliés, le moins récemment actif d'abord.
            "--place-mondes" => match v.parse::<u64>() {
                Ok(mb) if mb > 0 => minelib_relay::set_world_space(mb * 1_000_000),
                _ => {
                    eprintln!("--place-mondes : un nombre de Mo attendu ({v})");
                    std::process::exit(2);
                }
            },
            other => {
                eprintln!("option inconnue : {other}");
                std::process::exit(2);
            }
        }
        i += 2;
    }
    if !admins.is_empty() {
        std::fs::create_dir_all(&cfg.dir)?;
        minelib_relay::add_admins(&cfg.dir, &admins)?;
    }
    let key_path = cfg.dir.with_file_name("noeud.key");
    let h = minelib_relay::start(cfg)?;
    println!("Relais prêt sur {}", h.addr);
    println!(
        "Clé du relais (à comparer avec relay_key dans options.txt des joueurs) : {}",
        minelib_net::crypto::hex(&h.public)
    );
    // Joignable sans port ouvert, et inscrit dans le DHT.
    // (v326) Une machine sans joueur : le DHT de BitTorrent en plus des relais pkarr.
    minelib_p2p::allow_dht();
    // (v329) Son nœud prend une adresse chez un serveur TURN (`relais_turn.txt`) : les liens
    // des joueurs vers lui y passent aussi.
    minelib_p2p::enable_turn();
    let _node = if dht {
        // (v279) Identité iroh gardée d'un lancement à l'autre (`noeud.key`, comme le jeu) :
        // les jeux retiennent le nœud des relais où ils ont joué et le joignent
        // directement, et c'est lui qu'on écrit dans `relais_officiels.txt`. Avant, il
        // changeait à chaque lancement.
        let secret: [u8; 32] = std::fs::read(&key_path)
            .ok()
            .and_then(|b| b.try_into().ok())
            .unwrap_or_else(|| {
                let k: [u8; 32] = minelib_net::transport::generate_static().0;
                let _ = std::fs::write(&key_path, k);
                k
            });
        match minelib_p2p::Node::start_with(Some(secret)) {
            Ok(node) => {
                let node: &'static minelib_p2p::Node = Box::leak(Box::new(node));
                // Pseudos uniques avec les autres relais du DHT.
                h.set_whois(minelib_relay::federated_whois(node, h.public));
                // Relais fédérés : ses joueurs voient aussi les parties des autres relais.
                h.set_peer_servers(minelib_relay::federated_servers(node, h.public));
                let local = std::net::SocketAddr::from(([127, 0, 0, 1], h.addr.port()));
                node.serve(local);
                // (v320) Ses joueurs y viennent ensuite par des passeurs.
                h.set_own_node(node.id());
                minelib_p2p::publish(minelib_p2p::Ad {
                    node: node.id(),
                    noise: h.public,
                    name: "Minelib".into(),
                    since: minelib_p2p::now(),
                    seen: minelib_p2p::now(),
                });
                println!(
                    "Inscrit dans le DHT : les jeux en relay=auto le trouvent (nœud {}).",
                    minelib_net::crypto::hex(&node.id()[..8])
                );
                println!(
                    "Ligne pour relais_officiels.txt : {} {} Minelib",
                    minelib_net::crypto::hex(&node.id()),
                    minelib_net::crypto::hex(&h.public)
                );
                Some(node)
            }
            Err(e) => {
                eprintln!("iroh : {e:#}");
                None
            }
        }
    } else {
        None
    };
    // Arrêt (Ctrl+C, ou fenêtre fermée sous Windows) : les mondes qui ont un relais de
    // secours lui sont confiés tout de suite, sans attendre que leurs gardiens remarquent
    // le silence du relais.
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let on_ctrl_c = tx.clone();
    rt.spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            let _ = on_ctrl_c.send(());
        }
    });
    #[cfg(windows)]
    rt.spawn(async move {
        if let Ok(mut close) = tokio::signal::windows::ctrl_close() {
            close.recv().await;
            let _ = tx.send(());
        }
    });
    #[cfg(not(windows))]
    drop(tx);
    std::thread::spawn(move || rt.block_on(std::future::pending::<()>()));
    let _ = rx.recv();
    let n = h.release();
    if n > 0 {
        println!("Arrêt : {n} monde(s) confié(s) à leur relais de secours.");
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    Ok(())
}

/// (v320) `minelib-relay --passeur` : voir `minelib_relay::passeur_seul`.
fn passeur_main() -> Result<()> {
    use minelib_relay::passeur_seul::{Headless, Options};
    use std::time::{Duration, Instant};
    // (v326) Une machine sans joueur : le DHT de BitTorrent en plus des relais pkarr.
    minelib_p2p::allow_dht();
    let args: Vec<String> = std::env::args().collect();
    let mut dir = std::path::PathBuf::from("passeur_data");
    let mut ip: Option<std::net::Ipv4Addr> = None;
    let mut port: u16 = 0;
    let mut extra: Vec<minelib_p2p::Ad> = Vec::new();
    let mut invites: Vec<([u8; 32], [u8; 16])> = Vec::new();
    // (v347) Le journal réseau, envoyé aux relais qui le demandent, sauf `--sans-journaux`.
    let mut share_logs = true;
    let mut i = 1;
    while i < args.len() {
        let v = args.get(i + 1).cloned().unwrap_or_default();
        match args[i].as_str() {
            "--passeur" => {
                i += 1;
                continue;
            }
            "--sans-journaux" => {
                share_logs = false;
                i += 1;
                continue;
            }
            "--dir" => dir = v.into(),
            "--ip" => match v.parse() {
                Ok(a) => ip = Some(a),
                Err(_) => {
                    eprintln!("--ip : adresse IPv4 attendue ({v})");
                    std::process::exit(2);
                }
            },
            "--port" => port = v.parse().unwrap_or(0),
            "--relais" => extra.extend(minelib_p2p::parse_relays(&v)),
            "--invitation" => match minelib_p2p::parse_invitation(&v) {
                Some((secret, ad)) => {
                    invites.push((ad.noise, secret));
                    extra.push(ad);
                }
                None => {
                    eprintln!(
                        "--invitation : illisible (attendu : « <secret> <nœud> <clé> <nom> », \
                         donnée par /forwarder invite)"
                    );
                    std::process::exit(2);
                }
            },
            other => {
                eprintln!("option inconnue : {other}");
                std::process::exit(2);
            }
        }
        i += 2;
    }
    minelib_net::netlog::set_share(share_logs);
    std::fs::create_dir_all(&dir)?;
    let key_path = dir.join("noeud.key");
    let secret: [u8; 32] = std::fs::read(&key_path)
        .ok()
        .and_then(|b| b.try_into().ok())
        .unwrap_or_else(|| {
            let k: [u8; 32] = minelib_net::transport::generate_static().0;
            let _ = std::fs::write(&key_path, k);
            k
        });
    // (Nœud avec transport IP : l'adresse d'un passeur est faite pour être connue.)
    let node: &'static minelib_p2p::Node =
        Box::leak(Box::new(minelib_p2p::Node::start_with(Some(secret))?));
    let ip = match ip.or_else(|| node.public_ipv4(Duration::from_secs(20))) {
        Some(a) => a,
        None => {
            eprintln!(
                "Adresse publique introuvable : donne-la avec --ip (celle de la machine, vue \
                 d'internet ; ses ports UDP doivent être ouverts)."
            );
            std::process::exit(2);
        }
    };
    println!(
        "Passeur sans joueur : {ip}, ports UDP {} — dossier {}",
        if port == 0 {
            "au hasard".to_string()
        } else {
            format!("à partir de {port}")
        },
        dir.display()
    );
    let mut p = Headless::new(Options {
        dir: dir.clone(),
        ip,
        port,
    });
    for (key, secret) in invites {
        p.invite(key, secret);
    }
    let mut next_find = Instant::now();
    let mut next_say = Instant::now() + Duration::from_secs(60);
    loop {
        if Instant::now() >= next_find {
            next_find = Instant::now() + Duration::from_secs(600);
            let mut ads = minelib_p2p::official();
            ads.extend(extra.iter().cloned());
            match minelib_p2p::find_relays() {
                Ok(list) => ads.extend(list),
                Err(e) => log::warn!("liste des relais : {e:#}"),
            }
            for ad in ads {
                let (n, wait) = (ad.node, Duration::from_secs(10));
                p.add(
                    &ad.name,
                    ad.noise,
                    std::sync::Arc::new(move || {
                        let addr = node.tunnel_checked(n, wait)?.to_string();
                        // (Un relais d'une autre version du jeu n'en voudrait pas.)
                        match minelib_net::client::probe(&addr, wait) {
                            Ok((_, true)) => Ok(addr),
                            Ok((_, false)) => Err(anyhow::anyhow!("autre version du jeu")),
                            Err(e) => Err(e),
                        }
                    }),
                );
            }
        }
        p.tend();
        if Instant::now() >= next_say {
            next_say = Instant::now() + Duration::from_secs(60);
            println!(
                "Passeur : annoncé à {} relais sur {} connus.",
                p.joined(),
                p.relays()
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}
