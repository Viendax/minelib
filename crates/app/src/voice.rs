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

/// Clé publique renvoyée aux autres toutes les…
const HELLO_EVERY: f32 = 3.0;
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
    /// Clés de voix des autres : (époque, clé).
    theirs: HashMap<u64, (u32, [u8; 32])>,
    hello: f32,
    mic: Option<Mic>,
    mic_failed: bool,
    pending: Vec<f32>,
    phase: f64,
    /// Ce jeu émet en ce moment.
    pub talking: bool,
    /// Qui parle (temps depuis le dernier paquet).
    pub speaking: HashMap<u64, f32>,
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
            theirs: HashMap::new(),
            hello: 0.0,
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
        self.hello -= dt;
        if self.hello <= 0.0 {
            self.hello = HELLO_EVERY;
            self.send(
                everyone.to_vec(),
                &VoiceMsg::Hello {
                    pubkey: self.id.public,
                },
            );
        }
        // Quelqu'un ne doit plus m'entendre : nouvelle clé.
        let set: HashSet<u64> = listeners.iter().copied().collect();
        if self.given.iter().any(|g| !set.contains(g)) {
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
            VoiceMsg::Hello { pubkey } => {
                if self.pubkeys.insert(from, pubkey) != Some(pubkey) {
                    // Nouvelle clé publique : il faudra lui redonner la mienne.
                    self.given.remove(&from);
                }
            }
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

    #[test]
    fn resampling_48k_gives_16k() {
        let input = vec![0.5f32; 4800];
        let mut phase = 0.0;
        let out = resample(&input, 48_000, &mut phase);
        assert!((out.len() as i32 - 1600).abs() <= 1, "{}", out.len());
    }
}
