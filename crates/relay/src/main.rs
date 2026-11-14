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
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
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
    let h = minelib_relay::start(cfg)?;
    println!("Relais prêt sur {}", h.addr);
    println!(
        "Clé du relais (à comparer avec relay_key dans options.txt des joueurs) : {}",
        minelib_net::crypto::hex(&h.public)
    );
    // Joignable sans port ouvert, et inscrit dans le DHT.
    let _node = if dht {
        match minelib_p2p::Node::start() {
            Ok(node) => {
                let node: &'static minelib_p2p::Node = Box::leak(Box::new(node));
                // Pseudos uniques avec les autres relais du DHT.
                h.set_whois(minelib_relay::federated_whois(node, h.public));
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
    loop {
        std::thread::park();
    }
}
