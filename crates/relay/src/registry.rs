//! Registre public : journal qu'on ne peut que compléter, chaque ligne signée par le relais
//! et liée à la précédente par son empreinte. Il dit quel pseudo appartient à quelle clé,
//! et quels comptes sont bannis. Aucune donnée personnelle.

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Body {
    Register {
        pseudo: String,
        pubkey: [u8; 32],
    },
    /// Ban d'un compte, avec l'empreinte de la preuve (journal de partie rejouable).
    Ban {
        pubkey: [u8; 32],
        proof: [u8; 32],
    },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Entry {
    index: u64,
    prev: [u8; 32],
    time: u64,
    body: Body,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Signed {
    entry: Entry,
    signature: Vec<u8>,
}

pub struct Registry {
    path: PathBuf,
    signer: SigningKey,
    head: [u8; 32],
    next: u64,
    /// pseudo en minuscules → (pseudo, clé).
    pseudos: HashMap<String, (String, [u8; 32])>,
    banned: HashSet<[u8; 32]>,
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl Registry {
    /// Ouvre (ou crée) le registre et vérifie toute la chaîne.
    pub fn open(path: &Path, signer: SigningKey) -> Result<Self> {
        let mut reg = Self {
            path: path.to_path_buf(),
            signer,
            head: [0; 32],
            next: 0,
            pseudos: HashMap::new(),
            banned: HashSet::new(),
        };
        let mut data = Vec::new();
        if let Ok(mut f) = File::open(path) {
            f.read_to_end(&mut data)?;
        }
        let verify = reg.signer.verifying_key();
        let mut at = 0;
        while at < data.len() {
            if at + 4 > data.len() {
                return Err(anyhow!("registre tronqué"));
            }
            let len = u32::from_le_bytes(data[at..at + 4].try_into().unwrap()) as usize;
            let raw = data
                .get(at + 4..at + 4 + len)
                .ok_or_else(|| anyhow!("registre tronqué"))?;
            let signed: Signed = postcard::from_bytes(raw).context("ligne du registre")?;
            let body = postcard::to_allocvec(&signed.entry)?;
            let sig = Signature::from_slice(&signed.signature)?;
            verify
                .verify(&body, &sig)
                .map_err(|_| anyhow!("signature fausse à la ligne {}", signed.entry.index))?;
            if signed.entry.index != reg.next || signed.entry.prev != reg.head {
                return Err(anyhow!("chaîne rompue à la ligne {}", signed.entry.index));
            }
            reg.apply(&signed.entry.body);
            reg.head = Sha256::digest(raw).into();
            reg.next += 1;
            at += 4 + len;
        }
        Ok(reg)
    }

    fn apply(&mut self, body: &Body) {
        match body {
            Body::Register { pseudo, pubkey } => {
                self.pseudos
                    .insert(pseudo.to_ascii_lowercase(), (pseudo.clone(), *pubkey));
            }
            Body::Ban { pubkey, .. } => {
                self.banned.insert(*pubkey);
            }
        }
    }

    /// Ajoute une ligne signée ; renvoie son numéro.
    pub fn append(&mut self, body: Body) -> Result<u64> {
        if let Body::Register { pseudo, .. } = &body {
            if self.lookup(pseudo).is_some() {
                return Err(anyhow!("pseudo déjà inscrit"));
            }
        }
        let entry = Entry {
            index: self.next,
            prev: self.head,
            time: now(),
            body,
        };
        let signature = self
            .signer
            .sign(&postcard::to_allocvec(&entry)?)
            .to_bytes()
            .to_vec();
        let signed = Signed { entry, signature };
        let raw = postcard::to_allocvec(&signed)?;
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let mut frame = (raw.len() as u32).to_le_bytes().to_vec();
        frame.extend_from_slice(&raw);
        f.write_all(&frame)?;
        f.sync_data()?;
        self.apply(&signed.entry.body);
        self.head = Sha256::digest(&raw).into();
        self.next += 1;
        Ok(self.next - 1)
    }

    pub fn lookup(&self, pseudo: &str) -> Option<&(String, [u8; 32])> {
        self.pseudos.get(&pseudo.to_ascii_lowercase())
    }

    pub fn is_banned(&self, pubkey: &[u8; 32]) -> bool {
        self.banned.contains(pubkey)
    }

    pub fn accounts(&self) -> usize {
        self.pseudos.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tampering_is_detected() {
        let path = std::env::temp_dir().join(format!("minelib-reg-{}.log", rand::random::<u32>()));
        let key = SigningKey::from_bytes(&[3u8; 32]);
        let mut r = Registry::open(&path, key.clone()).unwrap();
        r.append(Body::Register {
            pseudo: "Alice".into(),
            pubkey: [1; 32],
        })
        .unwrap();
        r.append(Body::Register {
            pseudo: "Bob".into(),
            pubkey: [2; 32],
        })
        .unwrap();
        assert!(r
            .append(Body::Register {
                pseudo: "alice".into(),
                pubkey: [9; 32],
            })
            .is_err());
        r.append(Body::Ban {
            pubkey: [2; 32],
            proof: [0; 32],
        })
        .unwrap();
        let r = Registry::open(&path, key.clone()).unwrap();
        assert_eq!(r.accounts(), 2);
        assert!(r.is_banned(&[2; 32]));
        // Un octet changé (le pseudo « Bob » devient « Bab ») : refusé.
        let mut data = std::fs::read(&path).unwrap();
        let i = data.windows(3).position(|w| w == b"Bob").unwrap();
        data[i + 1] = b'a';
        std::fs::write(&path, &data).unwrap();
        assert!(Registry::open(&path, key).is_err());
        let _ = std::fs::remove_file(path);
    }
}
