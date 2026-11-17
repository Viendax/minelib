//! Chat vocal en jeu : micro (maintenir la touche « Micro »), chiffrement de bout en bout
//! (`minelib_net::voice`) et lecture des autres joueurs, chacun à son volume.
//!
//! FPS : on parle à ses coéquipiers. Construction : aux joueurs proches (le son baisse
//! avec la distance). La clé de voix n'est donnée qu'à eux : ni le relais, ni l'hôte, ni
//! les garants ne peuvent écouter.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use minelib_net::proto::Request;
use minelib_net::transport::{self, Inbound};
use minelib_net::voice::{self, Identity, VoiceMsg, FRAME, RATE};

/// Clé publique donnée à chaque nouveau voisin tout de suite, puis redonnée toutes les…
/// (v136 : avant, à tous les voisins toutes les 3 s ; dans une foule, le relais du monde
/// passait son temps à transmettre ces bonjours.)
const HELLO_EVERY: f32 = 30.0;
/// Un joueur « parle » encore tant qu'un paquet est arrivé depuis…
const SPEAKING_S: f32 = 0.3;

/// Micro ouvert (échantillons mono, à la fréquence du périphérique).
struct Mic {
    _stream: cpal::Stream,
    buf: Arc<Mutex<Vec<f32>>>,
    rate: u32,
}

fn open_mic() -> Option<Mic> {
    let host = cpal::default_host();
    let dev = host.default_input_device()?;
    let cfg = dev.default_input_config().ok()?;
    let channels = cfg.channels() as usize;
    let rate = cfg.sample_rate().0;
    let buf = Arc::new(Mutex::new(Vec::new()));
    let push = {
        let buf = buf.clone();
        move |mono: &mut dyn Iterator<Item = f32>| {
            if let Ok(mut b) = buf.lock() {
                b.extend(mono);
                // Pas plus de 2 s en attente.
                let max = rate as usize * 2;
                if b.len() > max {
                    let cut = b.len() - max;
                    b.drain(..cut);
                }
            }
        }
    };
    let err = |e| log::warn!("micro : {e}");
    let stream = match cfg.sample_format() {
        cpal::SampleFormat::F32 => {
            let push = push.clone();
            dev.build_input_stream(
                &cfg.config(),
                move |d: &[f32], _: &_| {
                    push(
                        &mut d
                            .chunks(channels)
                            .map(|c| c.iter().sum::<f32>() / channels as f32),
                    )
                },
                err,
                None,
            )
        }
        cpal::SampleFormat::I16 => {
            let push = push.clone();
            dev.build_input_stream(
                &cfg.config(),
                move |d: &[i16], _: &_| {
                    push(&mut d.chunks(channels).map(|c| {
                        c.iter().map(|&s| s as f32 / 32768.0).sum::<f32>() / channels as f32
                    }))
                },
                err,
                None,
            )
        }
        cpal::SampleFormat::U16 => dev.build_input_stream(
            &cfg.config(),
            move |d: &[u16], _: &_| {
                push(&mut d.chunks(channels).map(|c| {
                    c.iter()
                        .map(|&s| (s as f32 - 32768.0) / 32768.0)
                        .sum::<f32>()
                        / channels as f32
                }))
            },
            err,
            None,
        ),
        _ => return None,
    }
    .ok()?;
    stream.play().ok()?;
    log::info!("micro ouvert ({rate} Hz)");
    Some(Mic {
        _stream: stream,
        buf,
        rate,
    })
}

/// Rééchantillonne vers 16 kHz (interpolation linéaire).
fn resample(input: &[f32], from: u32, phase: &mut f64) -> Vec<f32> {
    if from == RATE {
        return input.to_vec();
    }
    let step = from as f64 / RATE as f64;
    let mut out = Vec::new();
    while (*phase as usize) + 1 < input.len() {
        let i = *phase as usize;
        let f = (*phase - i as f64) as f32;
        out.push(input[i] * (1.0 - f) + input[i + 1] * f);
        *phase += step;
    }
    *phase -= (input.len().saturating_sub(1)) as f64;
    out
}

pub struct Voice {
    link: Sender<Inbound>,
    id: Identity,
    /// Clés publiques des autres.
    pubkeys: HashMap<u64, [u8; 32]>,
    my_key: [u8; 32],
    epoch: u32,
    seq: u32,
    /// Qui a ma clé actuelle.
    given: HashSet<u64>,
    /// Un auditeur est parti : la clé doit changer (dès que `rekey_wait` est écoulé).
    rekey_due: bool,
    rekey_wait: f32,
    /// Clés de voix des autres : (époque, clé).
    theirs: HashMap<u64, (u32, [u8; 32])>,
    /// Bonjour (ma clé publique) déjà donné à chacun : temps avant de le redonner.
    hello: HashMap<u64, f32>,
    mic: Option<Mic>,
    mic_failed: bool,
    pending: Vec<f32>,
    phase: f64,
    /// Ce jeu émet en ce moment.
    pub talking: bool,
    /// Qui parle (temps depuis le dernier paquet).
    pub speaking: HashMap<u64, f32>,
}

/// Temps minimal entre deux changements de clé (s), selon le nombre d'auditeurs. Quand un
/// auditeur ne doit plus entendre, la clé change : aussitôt ou presque en petit groupe
/// (une équipe de FPS, quelques voisins), où cela ne coûte rien ; dans une foule, chaque
/// changement renvoie la clé à tous, alors on les espace (un relais malveillant garderait
/// l'ancien auditeur à l'écoute ce temps-là au plus).
fn rekey_gap(listeners: usize) -> f32 {
    match listeners {
        0..=16 => 0.5,
        17..=64 => 2.0,
        _ => 10.0,
    }
}

impl Voice {
    pub fn new(link: Sender<Inbound>) -> Self {
        Self {
            link,
            id: Identity::new(),
            pubkeys: HashMap::new(),
            my_key: voice::new_voice_key(),
            epoch: 1,
            seq: 0,
            given: HashSet::new(),
            rekey_due: false,
            rekey_wait: 0.0,
            theirs: HashMap::new(),
            hello: HashMap::new(),
            mic: None,
            mic_failed: false,
            pending: Vec::new(),
            phase: 0.0,
            talking: false,
            speaking: HashMap::new(),
        }
    }

    fn send(&self, to: Vec<u64>, msg: &VoiceMsg) {
        if to.is_empty() {
            return;
        }
        transport::push(
            &self.link,
            &Request::Voice {
                to,
                data: minelib_net::game::encode(msg),
            },
        );
    }

    /// À chaque image. `everyone` : tous les joueurs de la partie (clé publique) ;
    /// `listeners` : ceux qui doivent m'entendre ; `ptt` : touche Micro maintenue.
    pub fn update(&mut self, dt: f32, everyone: &[u64], listeners: &[u64], ptt: bool) {
        for t in self.speaking.values_mut() {
            *t += dt;
        }
        self.speaking.retain(|_, t| *t < SPEAKING_S);
        // Ma clé publique : à chaque nouveau voisin, puis de temps en temps.
        let near: HashSet<u64> = everyone.iter().copied().collect();
        self.hello.retain(|p, _| near.contains(p));
        let mut due = Vec::new();
        for &p in everyone {
            let t = self.hello.entry(p).or_insert(0.0);
            *t -= dt;
            if *t <= 0.0 {
                *t = HELLO_EVERY;
                due.push(p);
            }
        }
        self.send(
            due,
            &VoiceMsg::Hello {
                pubkey: self.id.public,
            },
        );
        // Quelqu'un ne doit plus m'entendre : nouvelle clé.
        let set: HashSet<u64> = listeners.iter().copied().collect();
        // (Il ne reçoit déjà plus mes messages ; la clé change presque aussitôt en petit
        // groupe, moins souvent dans une foule qui bouge : voir `rekey_gap`.)
        self.rekey_wait -= dt;
        if self.given.iter().any(|g| !set.contains(g)) {
            self.given.retain(|g| set.contains(g));
            self.rekey_due = true;
        }
        if self.rekey_due && self.rekey_wait <= 0.0 {
            self.rekey_due = false;
            self.rekey_wait = rekey_gap(listeners.len());
            self.my_key = voice::new_voice_key();
            self.epoch += 1;
            self.seq = 0;
            self.given.clear();
        }
        for &l in listeners {
            if self.given.contains(&l) {
                continue;
            }
            if let Some(pk) = self.pubkeys.get(&l) {
                let sealed = voice::seal(pk, &self.my_key);
                self.send(
                    vec![l],
                    &VoiceMsg::Key {
                        epoch: self.epoch,
                        sealed,
                    },
                );
                self.given.insert(l);
            }
        }
        // Micro : ouvert au premier appui.
        if ptt && self.mic.is_none() && !self.mic_failed {
            self.mic = open_mic();
            self.mic_failed = self.mic.is_none();
            if self.mic_failed {
                log::warn!("aucun micro utilisable");
            }
        }
        let Some(mic) = &self.mic else {
            self.talking = false;
            return;
        };
        let raw: Vec<f32> = mic
            .buf
            .lock()
            .map(|mut b| std::mem::take(&mut *b))
            .unwrap_or_default();
        self.talking = ptt;
        if !ptt {
            self.pending.clear();
            return;
        }
        let rate = mic.rate;
        let mut phase = self.phase;
        let resampled = resample(&raw, rate, &mut phase);
        self.phase = phase;
        self.pending.extend(resampled);
        let to: Vec<u64> = self.given.iter().copied().collect();
        while self.pending.len() >= FRAME {
            let frame: Vec<i16> = self
                .pending
                .drain(..FRAME)
                .map(|s| (s.clamp(-1.0, 1.0) * 32767.0) as i16)
                .collect();
            let plain = voice::encode(&frame);
            self.seq += 1;
            let ct = voice::encrypt(&self.my_key, self.epoch, self.seq, &plain);
            self.send(
                to.clone(),
                &VoiceMsg::Frame {
                    epoch: self.epoch,
                    seq: self.seq,
                    ct,
                },
            );
        }
    }

    /// Message vocal reçu de `from`, joué au volume `gain` (0 : coupé).
    pub fn receive(&mut self, from: u64, data: &[u8], gain: f32) {
        let Some(msg) = minelib_net::game::decode::<VoiceMsg>(data) else {
            return;
        };
        match msg {
            VoiceMsg::Hello { pubkey } => match self.pubkeys.insert(from, pubkey) {
                // Nouvelle clé publique : il faudra lui (re)donner ma clé de voix.
                None => {
                    self.given.remove(&from);
                }
                // Changée (son jeu a été relancé) : ma clé publique aussi, tout de suite.
                Some(old) if old != pubkey => {
                    self.given.remove(&from);
                    self.hello.remove(&from);
                }
                Some(_) => {}
            },
            VoiceMsg::Key { epoch, sealed } => {
                if let Some(k) = self.id.open(&sealed) {
                    self.theirs.insert(from, (epoch, k));
                }
            }
            VoiceMsg::Frame { epoch, seq, ct } => {
                let Some((e, k)) = self.theirs.get(&from) else {
                    return;
                };
                if *e != epoch {
                    return;
                }
                let Some(plain) = voice::decrypt(k, epoch, seq, &ct) else {
                    return;
                };
                self.speaking.insert(from, 0.0);
                if gain > 0.0 {
                    crate::audio::voice(from, voice::decode(&plain, FRAME), gain.min(2.0));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bonjours envoyés (destinataires de chaque `Hello`).
    fn hellos(rx: &std::sync::mpsc::Receiver<Inbound>) -> Vec<Vec<u64>> {
        let mut out = Vec::new();
        for m in rx.try_iter() {
            if let Inbound::Push(b) | Inbound::Loose(b, _) = m {
                if let Some(Request::Voice { to, data }) = minelib_net::game::decode::<Request>(&b)
                {
                    if matches!(
                        minelib_net::game::decode::<VoiceMsg>(&data),
                        Some(VoiceMsg::Hello { .. })
                    ) {
                        out.push(to);
                    }
                }
            }
        }
        out
    }

    /// Ma clé publique part à chaque nouveau voisin tout de suite, puis toutes les 30 s
    /// seulement (avant : à tous toutes les 3 s), et tout de suite à un voisin dont le jeu
    /// a été relancé.
    #[test]
    fn hellos_go_to_new_neighbours_then_rarely() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut v = Voice::new(tx);
        v.update(0.1, &[1, 2], &[], false);
        assert_eq!(hellos(&rx), vec![vec![1, 2]]);
        for _ in 0..50 {
            v.update(0.1, &[1, 2], &[], false);
        }
        assert!(hellos(&rx).is_empty(), "rien de nouveau en 5 s");
        v.update(0.1, &[1, 2, 3], &[], false);
        assert_eq!(hellos(&rx), vec![vec![3]], "le nouveau seulement");
        let pubkey = Identity::new().public;
        let hello = minelib_net::game::encode(&VoiceMsg::Hello { pubkey });
        v.receive(2, &hello, 1.0);
        v.update(0.1, &[1, 2, 3], &[], false);
        assert!(hellos(&rx).is_empty(), "première clé de 2 : rien à redire");
        let relaunched = Identity::new().public;
        let hello = minelib_net::game::encode(&VoiceMsg::Hello { pubkey: relaunched });
        v.receive(2, &hello, 1.0);
        v.update(0.1, &[1, 2, 3], &[], false);
        assert_eq!(hellos(&rx), vec![vec![2]], "son jeu a été relancé");
        for _ in 0..300 {
            v.update(0.1, &[1, 2, 3], &[], false);
        }
        let all: usize = hellos(&rx).iter().map(|t| t.len()).sum();
        assert_eq!(all, 3, "chacun une fois en 30 s");
    }

    #[test]
    fn resampling_48k_gives_16k() {
        let input = vec![0.5f32; 4800];
        let mut phase = 0.0;
        let out = resample(&input, 48_000, &mut phase);
        assert!((out.len() as i32 - 1600).abs() <= 1, "{}", out.len());
    }
}
