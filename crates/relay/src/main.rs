//! `minelib-relay` : relais Minelib à lancer sur un serveur (ou un PC) joignable par les
//! joueurs.
//!
//! Options : `--port <n>` (27650), `--dir <dossier>` (`relay_data`), `--pow <bits>` (20),
//! `--name <nom>`, `--no-dht` (ne pas s'inscrire dans le DHT).
//!
//! Par défaut, le relais est joignable sans port ouvert (iroh) et s'inscrit dans le DHT de
//! BitTorrent : les jeux réglés sur `relay=auto` le trouvent tout seuls.

use anyhow::Result;

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(
        "info,mainline=error,iroh=warn,iroh_relay=warn,iroh_quinn=warn,iroh_quinn_proto=warn,\
         quinn=warn,quinn_proto=warn,tracing=warn,portmapper=warn,netwatch=warn,pkarr=warn",
    ))
    .init();
    let mut cfg = minelib_relay::Config {
        dir: "relay_data".into(),
        bind: format!("0.0.0.0:{}", minelib_net::DEFAULT_PORT),
        pow_bits: 20,
        name: "Minelib".into(),
    };
    let mut args: Vec<String> = std::env::args().collect();
    let dht = !args.iter().any(|a| a == "--no-dht");
    args.retain(|a| a != "--no-dht");
    let mut i = 1;
    while i < args.len() {
        let v = args.get(i + 1).cloned().unwrap_or_default();
        match args[i].as_str() {
            "--port" => cfg.bind = format!("0.0.0.0:{v}"),
            "--dir" => cfg.dir = v.into(),
            "--pow" => cfg.pow_bits = v.parse().unwrap_or(20),
            "--name" => cfg.name = v,
            other => {
                eprintln!("option inconnue : {other}");
                std::process::exit(2);
            }
        }
        i += 2;
    }
    let key_path = cfg.dir.with_file_name("noeud.key");
    let h = minelib_relay::start(cfg)?;
    println!("Relais prêt sur {}", h.addr);
    println!(
        "Clé du relais (à comparer avec relay_key dans options.txt des joueurs) : {}",
        minelib_net::crypto::hex(&h.public)
    );
    // Joignable sans port ouvert, et inscrit dans le DHT.
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
