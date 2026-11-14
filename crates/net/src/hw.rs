//! Empreinte matérielle (ban de 3 mois d'une machine). Jamais envoyée telle quelle : le
//! jeu n'envoie que `sha256(relais ‖ empreinte)`, différente pour chaque relais, qui ne
//! permet pas de retrouver la machine ni de la suivre d'un relais à l'autre.

use sha2::{Digest, Sha256};

/// Identifiants stables de la machine (Windows : MachineGuid et numéro de série du
/// disque système ; Linux : /etc/machine-id).
fn raw() -> Vec<u8> {
    let mut out = Vec::new();
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const NO_WINDOW: u32 = 0x0800_0000;
        let run = |cmd: &str, args: &[&str]| {
            std::process::Command::new(cmd)
                .args(args)
                .creation_flags(NO_WINDOW)
                .output()
                .map(|o| o.stdout)
                .unwrap_or_default()
        };
        out.extend(run(
            "reg",
            &[
                "query",
                r"HKLM\SOFTWARE\Microsoft\Cryptography",
                "/v",
                "MachineGuid",
            ],
        ));
        out.extend(run("cmd", &["/C", "vol", "C:"]));
    }
    #[cfg(not(windows))]
    {
        out.extend(std::fs::read("/etc/machine-id").unwrap_or_default());
    }
    out
}

/// Empreinte de cette machine pour le relais de clé `relay`.
pub fn fingerprint_for(relay: &[u8; 32]) -> [u8; 32] {
    salted(relay, &raw())
}

pub fn salted(relay: &[u8; 32], raw: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"minelib/materiel");
    h.update(relay);
    h.update(raw);
    h.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fingerprint_differs_per_relay() {
        let a = salted(&[1; 32], b"machine");
        assert_eq!(a, salted(&[1; 32], b"machine"));
        assert_ne!(a, salted(&[2; 32], b"machine"));
    }
}
