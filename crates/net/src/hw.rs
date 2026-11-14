//! Empreinte matérielle (ban de 3 mois d'une machine), en plusieurs composants : changer
//! un seul composant (disque, carte réseau, réinstallation de Windows) ne suffit pas à
//! échapper au ban, il en faut au moins deux en commun. Rien n'est envoyé tel quel : pour
//! chaque composant, le jeu n'envoie que `sha256(relais ‖ composant)`, différent pour
//! chaque relais, qui ne permet ni de retrouver la machine ni de la suivre d'un relais à
//! l'autre.

use sha2::{Digest, Sha256};

/// Composants de la machine : identifiant Windows, UUID de la carte mère (SMBIOS),
/// numéro de série du BIOS, du disque système et du volume, processeur, cartes réseau
/// physiques. Linux : /etc/machine-id et identifiant DMI.
pub fn components() -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = Vec::new();
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const NO_WINDOW: u32 = 0x0800_0000;
        let script = "$ErrorActionPreference='SilentlyContinue';\
            (Get-ItemProperty 'HKLM:\\SOFTWARE\\Microsoft\\Cryptography').MachineGuid;\
            '|';(Get-CimInstance Win32_ComputerSystemProduct).UUID;\
            '|';(Get-CimInstance Win32_BIOS).SerialNumber;\
            '|';(Get-CimInstance Win32_BaseBoard).SerialNumber;\
            '|';(Get-CimInstance Win32_Processor).ProcessorId;\
            '|';(Get-PhysicalDisk | Select-Object -First 1).SerialNumber;\
            '|';(Get-CimInstance Win32_LogicalDisk -Filter \"DeviceID='C:'\").VolumeSerialNumber;\
            '|';(Get-CimInstance Win32_NetworkAdapter -Filter 'PhysicalAdapter=True' | \
                 Where-Object MACAddress | Select-Object -First 1).MACAddress";
        let text = std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .creation_flags(NO_WINDOW)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();
        out.extend(text.split('|').map(|p| p.trim().as_bytes().to_vec()));
    }
    #[cfg(not(windows))]
    {
        out.push(std::fs::read("/etc/machine-id").unwrap_or_default());
        out.push(std::fs::read("/sys/class/dmi/id/product_uuid").unwrap_or_default());
        out.push(std::fs::read("/sys/class/dmi/id/board_serial").unwrap_or_default());
    }
    // Composants vides ou génériques (« To be filled by O.E.M. », zéros) : ignorés.
    out.retain(|c| {
        let t = String::from_utf8_lossy(c).trim().to_lowercase();
        t.len() >= 4
            && !t.contains("o.e.m")
            && !t.contains("default")
            && !t.chars().all(|c| c == '0' || c == '-' || c == 'f')
    });
    out
}

/// Empreintes de cette machine pour le relais de clé `relay` (une par composant).
pub fn fingerprints_for(relay: &[u8; 32]) -> Vec<[u8; 32]> {
    components()
        .iter()
        .enumerate()
        .map(|(i, c)| salted(relay, i as u8, c))
        .collect()
}

pub fn salted(relay: &[u8; 32], kind: u8, raw: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"minelib/materiel/v2");
    h.update(relay);
    h.update([kind]);
    h.update(raw);
    h.finalize().into()
}

/// Deux empreintes désignent la même machine : au moins deux composants en commun (un
/// seul si l'une n'en a qu'un).
pub fn same_machine(a: &[[u8; 32]], b: &[[u8; 32]]) -> bool {
    let common = a.iter().filter(|x| b.contains(x)).count();
    let need = if a.len().min(b.len()) <= 1 { 1 } else { 2 };
    common >= need
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_changed_component_is_still_the_same_machine() {
        let r = [1u8; 32];
        let m: Vec<[u8; 32]> = (0..5).map(|i| salted(&r, i, b"composant")).collect();
        let mut new_disk = m.clone();
        new_disk[2] = salted(&r, 2, b"autre disque");
        assert!(same_machine(&m, &new_disk));
        let other: Vec<[u8; 32]> = (0..5).map(|i| salted(&r, i, b"autre pc")).collect();
        assert!(!same_machine(&m, &other));
        assert_ne!(
            salted(&[1; 32], 0, b"x"),
            salted(&[2; 32], 0, b"x"),
            "salé par relais"
        );
    }
}
