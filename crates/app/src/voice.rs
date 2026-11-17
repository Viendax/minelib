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
/// Construction (v137) : un message peut se perdre en route (un joueur pas encore relié au
/// relais de région de celui qui parle). Paquets reçus sans la clé pour les lire, ou
/// auditeur dont on n'a pas la clé publique : on le lui demande (`VoiceMsg::Ask`), au plus
/// toutes les…
const ASK_EVERY: f32 = 2.0;
/// On répond à une demande (bonjour, et clé de voix si l'on a parlé depuis moins de
/// `TALK_RECENT`) au plus toutes les…
const ANSWER_EVERY: f32 = 1.0;
const TALK_RECENT: f32 = 5.0;

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

/// Construction : numéro de voix d'un joueur, tiré de son pseudo (le même partout : par le
/// relais du monde comme par n'importe quel relais de région).
pub fn key_of(pseudo: &str) -> u64 {
    // FNV-1a.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in pseudo.to_ascii_lowercase().bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Construction : de quoi signer mon bonjour (clé du compte, son certificat, monde), et
/// vérifier ceux des autres (clé du relais du monde, monde).
pub struct Signer {
    pub key: minelib_net::ed25519_dalek::SigningKey,
    pub cert: minelib_net::sovereign::Cert,
    pub room: u64,
}

pub struct Voice {
    link: Sender<Inbound>,
    /// Construction (v137) : les messages ne partent pas d'ici, ils attendent que la partie
    /// les achemine (par pseudo, par le relais de région ou le relais du monde).
    out: Option<Vec<(Vec<u64>, Vec<u8>)>>,
    signer: Option<Signer>,
    /// Clé du relais du monde et monde : un bonjour qui arrive par un relais de région doit
    /// être signé par la clé du compte de celui qui le dit.
    verify: Option<([u8; 32], u64)>,
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
    /// À qui demander bonjour et clé (paquets reçus sans pouvoir les lire, auditeur sans
    /// clé publique), temps avant de pouvoir redemander à chacun, et avant de répondre
    /// encore à chacun.
    missing: HashSet<u64>,
    asked: HashMap<u64, f32>,
    answered: HashMap<u64, f32>,
    /// Temps depuis mon dernier paquet de voix.
    since_talk: f32,
    mic: Option<Mic>,
    mic_failed: bool,
    pending: Vec<f32>,
    phase: f64,
    /// Ce jeu émet en ce moment.
    pub talking: bool,
    /// Qui parle (temps depuis le dernier paquet).
    pub speaking: HashMap<u64, f32>,
    /// Essais : paquets de voix reçus et déchiffrés.
    #[cfg(test)]
    pub frames: usize,
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
            out: None,
            signer: None,
            verify: None,
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
            missing: HashSet::new(),
            asked: HashMap::new(),
            answered: HashMap::new(),
            since_talk: f32::MAX,
            mic: None,
            mic_failed: false,
            pending: Vec::new(),
            phase: 0.0,
            talking: false,
            speaking: HashMap::new(),
            #[cfg(test)]
            frames: 0,
        }
    }

    /// Essais : un paquet de voix (du silence) à ceux qui ont ma clé, comme si je parlais.
    #[cfg(test)]
    pub fn talk_once(&mut self) {
        let to: Vec<u64> = self.given.iter().copied().collect();
        let plain = voice::encode(&[0i16; FRAME]);
        self.seq += 1;
        self.since_talk = 0.0;
        let ct = voice::encrypt(&self.my_key, self.epoch, self.seq, &plain);
        self.send(
            to,
            &VoiceMsg::Frame {
                epoch: self.epoch,
                seq: self.seq,
                ct,
            },
        );
    }

    /// Clés publiques connues des autres, clés de voix reçues d'eux.
    #[cfg(test)]
    pub fn known(&self) -> (usize, usize) {
        (self.pubkeys.len(), self.theirs.len())
    }

    /// Construction : les messages attendent d'être acheminés par la partie (`take_out`).
    pub fn route_by_pseudo(&mut self) {
        self.out.get_or_insert_with(Vec::new);
    }

    /// Construction : ce qui attend d'être acheminé (numéros de voix, message).
    pub fn take_out(&mut self) -> Vec<(Vec<u64>, Vec<u8>)> {
        self.out.as_mut().map(std::mem::take).unwrap_or_default()
    }

    /// Construction : mon bonjour signé avec la clé de mon compte.
    pub fn set_signer(&mut self, signer: Signer) {
        if self.signer.as_ref().is_none_or(|s| s.cert != signer.cert) {
            // (Nouveau certificat : ma clé publique repart signée.)
            self.hello.clear();
        }
        self.signer = Some(signer);
    }

    /// Construction : vérifier les bonjours avec la clé du relais du monde `root`.
    pub fn set_verify(&mut self, root: [u8; 32], room: u64) {
        self.verify = Some((root, room));
    }

    fn send(&mut self, to: Vec<u64>, msg: &VoiceMsg) {
        if to.is_empty() {
            return;
        }
        let data = minelib_net::game::encode(msg);
        if let Some(out) = &mut self.out {
            out.push((to, data));
            return;
        }
        transport::push(&self.link, &Request::Voice { to, data });
    }

    /// Mon bonjour : signé si je peux (construction), sinon ma seule clé publique.
    fn hello_msg(&self) -> VoiceMsg {
        match &self.signer {
            Some(s) => voice::signed_hello(&s.key, s.cert.clone(), s.room, self.id.public),
            None => VoiceMsg::Hello {
                pubkey: self.id.public,
            },
        }
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
        // Un message perdu en route : je redemande (voir `ASK_EVERY`).
        self.since_talk = (self.since_talk + dt).min(f32::MAX / 2.0);
        for t in self.asked.values_mut().chain(self.answered.values_mut()) {
            *t -= dt;
        }
        self.asked.retain(|p, t| *t > 0.0 && near.contains(p));
        self.answered.retain(|_, t| *t > 0.0);
        if self.since_talk < TALK_RECENT {
            for l in listeners {
                if !self.pubkeys.contains_key(l) {
                    self.missing.insert(*l);
                }
            }
        }
        let mut ask = Vec::new();
        for p in std::mem::take(&mut self.missing) {
            if near.contains(&p) && !self.asked.contains_key(&p) {
                self.asked.insert(p, ASK_EVERY);
                ask.push(p);
            }
        }
        self.send(ask, &VoiceMsg::Ask);
        let hello = self.hello_msg();
        self.send(due, &hello);
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
            self.since_talk = 0.0;
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

    /// Message vocal reçu de `from`, joué au volume `gain` (0 : coupé). `who` : son pseudo
    /// (construction), et si le message est passé par un relais de région (il peut mentir :
    /// un bonjour n'y vaut que signé par la clé du compte de `who`).
    pub fn receive(&mut self, from: u64, who: Option<(&str, bool)>, data: &[u8], gain: f32) {
        let Some(mut msg) = minelib_net::game::decode::<VoiceMsg>(data) else {
            return;
        };
        // Un bonjour arrivé par un relais de région ne vaut que signé par la clé du compte de
        // celui qui le dit (vérifié avec la clé du relais du monde). Par le relais du monde
        // (de confiance, comme en FPS), signé ou non, il compte ; signé, il est vérifié si
        // l'on peut.
        let relayed = who.is_some_and(|w| w.1);
        if let VoiceMsg::Signed { pubkey, .. } = &msg {
            let checked = match (who, self.verify) {
                (Some((who, _)), Some((root, room))) => voice::check_hello(&msg, who, &root, room),
                (_, None) if !relayed => Some(*pubkey),
                _ => None,
            };
            match checked {
                Some(pubkey) => msg = VoiceMsg::Hello { pubkey },
                None => return,
            }
        } else if matches!(msg, VoiceMsg::Hello { .. }) && relayed {
            // Bonjour non signé passé par un relais de région : refusé.
            return;
        }
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
            // Il lui manque mon bonjour ou ma clé (perdus en route) : mon bonjour à la
            // prochaine image, ma clé aussi si je parle (au plus une fois par
            // `ANSWER_EVERY` : la demande n'est pas signée, un relais pourrait en inventer).
            VoiceMsg::Ask => {
                if self.answered.contains_key(&from) {
                    return;
                }
                self.answered.insert(from, ANSWER_EVERY);
                self.hello.remove(&from);
                if self.since_talk < TALK_RECENT {
                    self.given.remove(&from);
                }
            }
            // (Déjà vérifié et changé en bonjour plus haut.)
            VoiceMsg::Signed { .. } => {}
            VoiceMsg::Key { epoch, sealed } => {
                if let Some(k) = self.id.open(&sealed) {
                    self.theirs.insert(from, (epoch, k));
                }
            }
            VoiceMsg::Frame { epoch, seq, ct } => {
                // (Pas sa clé, ou plus la bonne : je la lui redemande.)
                let Some((e, k)) = self.theirs.get(&from) else {
                    self.missing.insert(from);
                    return;
                };
                if *e != epoch {
                    if epoch > *e {
                        self.missing.insert(from);
                    }
                    return;
                }
                let Some(plain) = voice::decrypt(k, epoch, seq, &ct) else {
                    return;
                };
                self.speaking.insert(from, 0.0);
                #[cfg(test)]
                {
                    self.frames += 1;
                }
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
        v.receive(2, None, &hello, 1.0);
        v.update(0.1, &[1, 2, 3], &[], false);
        assert!(hellos(&rx).is_empty(), "première clé de 2 : rien à redire");
        let relaunched = Identity::new().public;
        let hello = minelib_net::game::encode(&VoiceMsg::Hello { pubkey: relaunched });
        v.receive(2, None, &hello, 1.0);
        v.update(0.1, &[1, 2, 3], &[], false);
        assert_eq!(hellos(&rx), vec![vec![2]], "son jeu a été relancé");
        for _ in 0..300 {
            v.update(0.1, &[1, 2, 3], &[], false);
        }
        let all: usize = hellos(&rx).iter().map(|t| t.len()).sum();
        assert_eq!(all, 3, "chacun une fois en 30 s");
    }

    /// Construction (v137) : les messages perdus en route (un joueur pas encore relié au
    /// relais de région de l'autre) se rattrapent. Bonjours perdus des deux côtés : celui qui
    /// parle demande la clé publique de son auditeur ; clé de voix perdue : l'auditeur reçoit
    /// des paquets illisibles et la redemande.
    #[test]
    fn lost_hellos_and_keys_are_asked_again() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut a = Voice::new(tx.clone());
        let mut b = Voice::new(tx);
        a.route_by_pseudo();
        b.route_by_pseudo();
        let (ka, kb) = (1u64, 2u64);
        // (Achemine ce que `from` a à envoyer vers `to`, ou le perd.)
        fn pass(from: &mut Voice, key: u64, to: &mut Voice, lose: bool) -> usize {
            let out = from.take_out();
            let n = out.len();
            if !lose {
                for (_, d) in out {
                    to.receive(key, None, &d, 0.0);
                }
            }
            n
        }
        // Les deux bonjours se perdent.
        a.update(0.1, &[kb], &[kb], false);
        b.update(0.1, &[ka], &[ka], false);
        pass(&mut a, ka, &mut b, true);
        pass(&mut b, kb, &mut a, true);
        // A parle : il n'a pas la clé publique de B, il la lui demande ; B redonne son
        // bonjour ; A lui donne sa clé de voix, mais elle se perd.
        a.talk_once();
        a.update(0.1, &[kb], &[kb], false);
        assert!(pass(&mut a, ka, &mut b, false) > 0, "demande de A");
        b.update(0.1, &[ka], &[ka], false);
        pass(&mut b, kb, &mut a, false);
        assert_eq!(a.known().0, 1, "A a la clé publique de B");
        a.update(0.1, &[kb], &[kb], false);
        pass(&mut a, ka, &mut b, true);
        // A parle encore : B reçoit des paquets illisibles et redemande la clé ; A la redonne.
        a.talk_once();
        pass(&mut a, ka, &mut b, false);
        assert_eq!(b.frames, 0);
        b.update(0.1, &[ka], &[ka], false);
        pass(&mut b, kb, &mut a, false);
        a.update(0.1, &[kb], &[kb], false);
        pass(&mut a, ka, &mut b, false);
        a.talk_once();
        pass(&mut a, ka, &mut b, false);
        assert_eq!(b.frames, 1, "B entend A");
        // Une demande par `ASK_EVERY` au plus, une réponse par `ANSWER_EVERY` au plus.
        for _ in 0..5 {
            b.receive(ka, None, &minelib_net::game::encode(&VoiceMsg::Ask), 0.0);
        }
        b.update(0.1, &[ka], &[ka], false);
        let answers = b.take_out();
        assert_eq!(answers.len(), 1, "un seul bonjour en réponse");
    }

    /// Construction (v137) : un bonjour arrivé par un relais de région ne vaut que signé par
    /// la clé du compte de celui qui le dit (certificat du relais du monde) ; un relais qui
    /// mettrait sa propre clé, ou attribuerait le bonjour d'un joueur à un autre, est ignoré.
    #[test]
    fn hellos_through_a_region_relay_must_be_signed_by_the_account() {
        use minelib_net::ed25519_dalek::SigningKey;
        let root = SigningKey::from_bytes(&[3; 32]);
        let room = 42;
        let account = |name: &str, seed: u8| {
            let key = SigningKey::from_bytes(&[seed; 32]);
            let cert = minelib_net::sovereign::Cert::issue(
                &root,
                room,
                name,
                key.verifying_key().to_bytes(),
                u64::MAX / 2,
            );
            Signer { key, cert, room }
        };
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut alice = Voice::new(tx.clone());
        alice.route_by_pseudo();
        alice.set_signer(account("Alice", 1));
        alice.set_verify(root.verifying_key().to_bytes(), room);
        let mut bob = Voice::new(tx);
        bob.route_by_pseudo();
        bob.set_signer(account("Bob", 2));
        bob.set_verify(root.verifying_key().to_bytes(), room);
        let (ka, kb) = (key_of("Alice"), key_of("Bob"));
        alice.update(0.1, &[kb], &[kb], false);
        let out = alice.take_out();
        assert_eq!(out.len(), 1);
        let hello = &out[0].1;
        // Attribué à un autre par le relais : ignoré.
        bob.receive(key_of("Carol"), Some(("Carol", true)), hello, 1.0);
        assert!(!bob.pubkeys.contains_key(&key_of("Carol")));
        // Le vrai : accepté.
        bob.receive(ka, Some(("Alice", true)), hello, 1.0);
        assert!(bob.pubkeys.contains_key(&ka));
        // Un relais qui met sa propre clé publique dans un bonjour non signé : refusé ;
        // le même par le relais du monde (de confiance, comme en FPS) : accepté.
        let fake = minelib_net::game::encode(&VoiceMsg::Hello {
            pubkey: Identity::new().public,
        });
        let before = bob.pubkeys[&ka];
        bob.receive(ka, Some(("Alice", true)), &fake, 1.0);
        assert_eq!(bob.pubkeys[&ka], before, "par un relais de région : refusé");
        bob.receive(ka, Some(("Alice", false)), &fake, 1.0);
        assert_ne!(bob.pubkeys[&ka], before, "par le relais du monde : accepté");
        // Signé par une autre clé que celle du certificat (relais qui garde le certificat
        // d'Alice et signe lui-même sa clé) : refusé.
        let liar = SigningKey::from_bytes(&[9; 32]);
        let forged = match minelib_net::game::decode::<VoiceMsg>(hello) {
            Some(VoiceMsg::Signed { cert, .. }) => {
                minelib_net::voice::signed_hello(&liar, cert, room, Identity::new().public)
            }
            _ => unreachable!(),
        };
        let now = bob.pubkeys[&ka];
        bob.receive(
            ka,
            Some(("Alice", true)),
            &minelib_net::game::encode(&forged),
            1.0,
        );
        assert_eq!(bob.pubkeys[&ka], now);
    }

    #[test]
    fn resampling_48k_gives_16k() {
        let input = vec![0.5f32; 4800];
        let mut phase = 0.0;
        let out = resample(&input, 48_000, &mut phase);
        assert!((out.len() as i32 - 1600).abs() <= 1, "{}", out.len());
    }
}
