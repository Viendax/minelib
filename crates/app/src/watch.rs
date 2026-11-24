//! Regarder ensemble (v151) : en construction en ligne, les écrans vidéo et leurs vidéos
//! YouTube sont partagés entre joueurs proches.
//!
//! Aucun nouveau message pour les relais : ceux-ci voyagent comme la voix (par pseudo, par
//! le relais de région, de province ou du monde), précédés de [`MAGIC`] ; le jeu de la voix
//! ne sait pas les lire et les ignore.
//!
//! - **Écrans.** Celui qui a posé un écran le montre aux joueurs à moins de [`RANGE`] blocs
//!   (toutes les 2 s, et tout de suite quand ses écrans changent). Chez eux il apparaît, se
//!   touche et arrête le joueur comme les leurs ; lui seul le casse. Plus annoncé depuis
//!   6 s, il disparaît. Un écran n'est accepté que près de celui qui l'annonce.
//! - **Seule une vidéo YouTube se partage** (son numéro de 11 caractères), jamais une
//!   adresse : personne ne peut ouvrir une autre page chez les autres. Une recherche, la
//!   page d'accueil ou un autre site restent sur l'écran de celui qui les ouvre.
//! - **Vidéo partagée.** Chaque écran a une vidéo : numéro, temps, pause, et un rang (`seq`,
//!   puis le pseudo de celui qui l'a donnée). Celui qui lance, met en pause, avance, recule
//!   ou change de vidéo sur sa page la donne avec le rang suivant ; chacun garde la plus
//!   récente et y amène sa page (vidéo ouverte, temps et pause corrigés). Une page en retard
//!   de plus de 4 s (chargement) est recalée. Pendant une pub YouTube, rien n'est lu ni
//!   corrigé. Qui reçoit plus ancien que ce qu'il a répond avec le sien.
//! - **(v253) File d'attente.** Chaque écran a sa file, partagée avec la vidéo (même rang) :
//!   en ligne, cliquer une vidéo sur le site de YouTube l'y ajoute (ou l'ouvre s'il n'y a
//!   rien en cours), la page du lecteur la montre à droite et chacun peut en retirer une.
//!   Quand la vidéo finit, celui qui le voit passe à la suivante pour tous. Seuls des numéros
//!   de vidéo voyagent ; titres et images, chacun les charge chez YouTube.
//! - **(v246) Shorts.** Ils restent sur le site de YouTube, chez chacun : glisser au Short
//!   suivant change la vidéo de l'écran, et les autres sont amenés au même Short
//!   (`youtube.com/shorts/<numéro>`) dans leur propre navigateur. On partage le numéro du
//!   Short, jamais les images de la page.

use std::collections::{HashMap, HashSet};

use glam::Vec3;
use serde::{Deserialize, Serialize};

use crate::screen::Placed;

/// Début des messages de cette partie dans le canal de la voix. Lu comme un message de la
/// voix, `0xFF` puis `M` est un numéro de variante que la voix n'a pas : ignoré. `2` : avec
/// la taille des écrans (v160) ; `3` : (v246) une vidéo peut être un Short ; `4` : (v253)
/// avec la file d'attente ; `5` : (v291) avec les vidéos déjà vues (la playlist entière).
/// Un jeu plus ancien ignore ces messages, et inversement.
pub const MAGIC: [u8; 4] = [0xFF, b'M', b'T', b'5'];
/// (v253) Au plus tant de vidéos ajoutées dans la file d'un écran.
pub const MAX_FILE: usize = 30;
/// (v291) Au plus tant de vidéos dans la playlist d'un écran, sans celle en cours (déjà
/// vues et à suivre) : au-delà, les plus anciennes vues s'en vont.
pub const MAX_LISTE: usize = 60;
/// Un écran est montré aux joueurs à moins de tant de blocs de lui.
pub const RANGE: f32 = 64.0;
/// Un écran annoncé plus loin que ça de celui qui l'annonce est refusé.
const ACCEPT_RANGE: f32 = 96.0;
/// Annonce de ses écrans, et durée de vie de ceux des autres sans nouvelle annonce (s).
const ANNOUNCE_S: f32 = 2.0;
const EXPIRE_S: f32 = 6.0;
/// Au plus tant d'écrans par annonce, et d'écrans des autres montrés.
const MAX_PER_MSG: usize = 24;
const MAX_REMOTE: usize = 64;
/// Au plus tant de destinataires par message (le relais en sert 256).
const MAX_TO: usize = 200;
/// Taille d'un message (le relais en prend 4096 octets).
const MAX_BYTES: usize = 4000;
/// Un saut de la vidéo plus grand que ça est un geste (avancer, reculer).
const JUMP_FWD_S: f32 = 2.0;
const JUMP_BACK_S: f32 = 1.5;
/// Une page qui s'écarte de plus de tant de la vidéo partagée est recalée (de plus de
/// `FINE_S` une fois, quand elle vient d'ouvrir la vidéo).
const DRIFT_S: f32 = 3.0;
const FINE_S: f32 = 1.0;
/// Le temps qu'une page se mette à jour après une vidéo ouverte, ou un temps corrigé : ce
/// qu'elle rapporte alors n'est pas un geste du joueur.
const SETTLE_OPEN_S: f32 = 5.0;
const SETTLE_EVAL_S: f32 = 1.5;
/// Au plus une réponse par écran tant de secondes à qui a plus ancien.
const CORRECT_S: f32 = 2.0;
/// Rangs : jamais au-delà, et pas plus de tant d'un coup (un tricheur ne bloque pas
/// l'écran en donnant le plus grand).
const SEQ_MAX: u32 = 1 << 30;
const SEQ_JUMP: u32 = 1000;
/// Temps de vidéo plausible (100 h).
const MAX_T: f32 = 360_000.0;

/// Vidéo partagée d'un écran.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Video {
    /// Numéro de la vidéo YouTube (11 caractères).
    pub id: String,
    /// Temps de la vidéo (secondes) quand le message part.
    pub t: f32,
    pub paused: bool,
    /// Rang : le plus grand l'emporte, puis le pseudo `by` (le plus grand).
    pub seq: u32,
    /// Qui l'a donnée.
    pub by: String,
    /// (v246) Un Short : il s'ouvre sur le site de YouTube, pas dans le lecteur.
    pub short: bool,
    /// (v253) La file d'attente de l'écran (numéros de vidéos, dans l'ordre).
    pub queue: Vec<String>,
    /// (v291) Les vidéos déjà vues de la playlist, de la plus ancienne à la dernière : la
    /// playlist entière est `lus`, puis `id` (en cours), puis `queue`.
    pub lus: Vec<String>,
}

impl Video {
    fn newer_than(&self, o: &Video) -> bool {
        (self.seq, self.by.as_str()) > (o.seq, o.by.as_str())
    }

    fn valid(&self) -> bool {
        valid_id(&self.id)
            && self.t.is_finite()
            && (0.0..=MAX_T).contains(&self.t)
            && self.seq < SEQ_MAX
            && !self.by.is_empty()
            && self.by.len() <= 32
            && self.queue.len() + self.lus.len() <= MAX_LISTE
            && self.queue.iter().chain(&self.lus).all(|v| valid_id(v))
    }

    /// (v291) La playlist entière (vues, en cours, à suivre) et le rang de celle en cours.
    fn liste(&self) -> (Vec<String>, usize) {
        let mut l = self.lus.clone();
        l.push(self.id.clone());
        l.extend(self.queue.iter().cloned());
        (l, self.lus.len())
    }

    /// (v291) Remet la playlist `l`, la vidéo en cours au rang `pos` (les vues avant, les
    /// à suivre après). Au-delà de `MAX_LISTE`, les plus anciennes vues s'en vont.
    fn set_liste(&mut self, mut l: Vec<String>, pos: usize) {
        if pos >= l.len() {
            return;
        }
        self.queue = l.split_off(pos + 1);
        if let Some(v) = l.pop() {
            self.id = v;
        }
        self.lus = l;
        self.rogne();
    }

    /// (v291) La playlist tient dans `MAX_LISTE` : les plus anciennes vues s'en vont.
    fn rogne(&mut self) {
        let trop = (self.lus.len() + self.queue.len()).saturating_sub(MAX_LISTE);
        self.lus.drain(..trop.min(self.lus.len()));
        self.queue.truncate(MAX_LISTE);
    }
}

/// Un écran tel qu'il part chez les autres.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Shared {
    block: [i32; 3],
    normal: [i32; 3],
    facing: [i32; 3],
    /// Taille (indice dans `crate::screen::SIZES`).
    size: u8,
    video: Option<Video>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum Msg {
    /// Les écrans de celui qui les a posés, près de celui qui reçoit (tous : un écran
    /// absent a été retiré).
    Screens(Vec<Shared>),
    /// La vidéo de l'écran `screen`.
    Video { screen: u32, video: Video },
}

/// Ce qu'il faut faire à la page d'un écran pour l'amener à la vidéo partagée.
#[derive(Clone, Debug, PartialEq)]
pub enum Act {
    /// Ouvrir cette adresse (une vidéo YouTube).
    Open(u32, String),
    /// Lancer ce script (temps, pause).
    Eval(u32, String),
}

/// État rapporté par une page.
#[derive(Clone, Debug, PartialEq)]
pub struct Page {
    /// Vidéo YouTube ouverte (d'après l'adresse), s'il y en a une.
    pub video: Option<String>,
    /// Temps de la vidéo ; négatif s'il n'est pas sûr (pas de vidéo, changement en cours).
    pub t: f32,
    pub paused: bool,
    /// Une pub YouTube passe.
    pub ad: bool,
    /// (v246) La page est un Short du site de YouTube.
    pub short: bool,
    /// (v253) La vidéo est finie.
    pub ended: bool,
    /// (v253) La file que montre la page du lecteur (numéros séparés par des virgules) ;
    /// `None` : la page n'en montre pas (ce n'est pas le lecteur).
    pub file: Option<String>,
}

/// Vidéo partagée d'un écran, telle qu'on la connaît ici.
struct Track {
    video: Video,
    /// Depuis combien de temps `video.t` est le temps de la vidéo (secondes).
    age: f32,
    /// Rang déjà amené dans la page, et dernière vidéo ouverte pour lui.
    applied: Option<(u32, String)>,
    applied_video: Option<String>,
    /// La page se met à jour : ce qu'elle rapporte n'est pas un geste (secondes).
    settle: f32,
    /// Depuis la dernière réponse à qui avait plus ancien (secondes).
    corrected: f32,
    /// Vidéo changée ici avant que sa page en donne le temps : il partira dès qu'elle le
    /// donne.
    guess: bool,
    /// Vidéo ouverte pour elle : la page sera recalée au plus près une fois chargée.
    fine: bool,
    /// (v291) Personne d'autre près de l'écran (à la dernière mise à jour) : quitter la
    /// vidéo pour un autre site la met alors en pause ; avec d'autres, elle continue pour eux.
    seul: bool,
}

impl Track {
    fn now(&self) -> f32 {
        self.video.t + if self.video.paused { 0.0 } else { self.age }
    }

    fn at_now(&self) -> Video {
        Video {
            t: self.now().min(MAX_T),
            ..self.video.clone()
        }
    }
}

/// Dernier état rapporté par une page, et depuis combien de temps (secondes).
struct Seen {
    page: Page,
    age: f32,
}

/// Écrans d'un autre joueur.
struct Remote {
    owner: String,
    screens: Vec<Placed>,
    age: f32,
}

/// Regarder ensemble : écrans des autres et vidéos partagées.
pub struct Party {
    tracks: HashMap<u32, Track>,
    pages: HashMap<u32, Seen>,
    /// Écrans des autres, par pseudo (minuscules).
    remote: HashMap<String, Remote>,
    /// Écrans annoncés à chaque joueur la dernière fois (pseudo en minuscules).
    told: HashMap<String, Vec<u32>>,
    announce_in: f32,
    /// Ses écrans ont changé : annoncés tout de suite.
    dirty: bool,
    /// Vidéos changées ici, à dire aux joueurs proches.
    changed: Vec<u32>,
    /// Réponses à faire : (joueur, écran).
    corrections: Vec<(String, u32)>,
    out: Vec<(Vec<String>, Vec<u8>)>,
}

impl Party {
    pub fn new() -> Self {
        Self {
            tracks: HashMap::new(),
            pages: HashMap::new(),
            remote: HashMap::new(),
            told: HashMap::new(),
            announce_in: 0.0,
            dirty: false,
            changed: Vec::new(),
            corrections: Vec::new(),
            out: Vec::new(),
        }
    }

    /// Hors ligne : on oublie tout.
    pub fn clear(&mut self) {
        if !self.tracks.is_empty() || !self.remote.is_empty() || !self.told.is_empty() {
            *self = Self::new();
        }
    }

    /// Écrans des autres joueurs à montrer, avec leur propriétaire (sans ceux qui sont à la
    /// place d'un des siens ; au plus 64).
    pub fn screens(&self, local: &[Placed]) -> Vec<(Placed, String)> {
        let mut ids: HashSet<u32> = local.iter().map(Placed::id).collect();
        let mut owners: Vec<&Remote> = self.remote.values().collect();
        owners.sort_by(|a, b| a.owner.cmp(&b.owner));
        let mut out = Vec::new();
        for r in owners {
            for s in &r.screens {
                if out.len() < MAX_REMOTE && ids.insert(s.id()) {
                    out.push((*s, r.owner.clone()));
                }
            }
        }
        out
    }

    /// Ses écrans ont changé (posé, cassé) : les joueurs proches le savent tout de suite.
    pub fn screens_changed(&mut self) {
        self.dirty = true;
    }

    /// La page de l'écran `id` rapporte son état (`{"u":…,"t":…,"p":…,"a":…,"i":…}`). Un
    /// geste du joueur (vidéo lancée, mise en pause, avancée, changée) devient la vidéo
    /// partagée de l'écran, avec le rang suivant.
    pub fn observe(&mut self, id: u32, json: &str, me: &str) {
        let Some(page) = parse_page(json) else {
            return;
        };
        let prev = self.pages.insert(
            id,
            Seen {
                page: page.clone(),
                age: 0.0,
            },
        );
        // (v253) La vidéo partagée est finie ici : la suivante de la file, pour tous.
        if page.ended
            && self.tracks.get(&id).is_some_and(|t| {
                page.video.as_deref() == Some(t.video.id.as_str()) && !t.video.queue.is_empty()
            })
        {
            self.advance(id, me);
            return;
        }
        // La page se met à jour : ce rapport n'est pas un geste. Elle a fini dès qu'elle
        // montre la vidéo partagée (même vidéo, même pause, même temps ; une vidéo qu'on
        // vient d'ouvrir est recalée ensuite).
        if let Some(t) = self.tracks.get_mut(&id).filter(|t| t.settle > 0.0) {
            let done = !page.ad
                && page.t >= 0.0
                && page.video.as_deref() == Some(t.video.id.as_str())
                && page.paused == t.video.paused
                && (t.fine || (page.t - t.now()).abs() <= FINE_S + 0.5);
            if done {
                t.settle = 0.0;
            }
            return;
        }
        if page.ad {
            return;
        }
        let track = self.tracks.get(&id);
        let shared = track.map(|t| t.video.id.clone());
        // Vidéo changée ici avant d'en avoir le temps : on le donne dès qu'on l'a.
        if track.is_some_and(|t| t.guess) && page.video == shared && page.t >= 0.0 {
            if let Some(v) = page.video.clone() {
                self.local(id, v, page.t, page.paused, page.short, me);
            }
            return;
        }
        let before = prev.as_ref().and_then(|p| p.page.video.clone());
        let Some(v) = page.video.clone() else {
            // Accueil, recherche, autre site : rien ne se partage. (v291) Mais quitter la
            // vidéo partagée pour un autre site la met en pause là où elle en était (demande
            // du propriétaire) : un clic sur elle dans la playlist la reprend à cet endroit.
            // Seulement sans personne d'autre près de l'écran : avec d'autres, elle continue
            // pour eux, et le clic ramène la page au moment où ils en sont.
            if before.is_some() && before == shared {
                if let Some(t) = self.tracks.get(&id).filter(|t| !t.video.paused && t.seul) {
                    let (vid, at, short) = (t.video.id.clone(), t.now(), t.video.short);
                    log::info!("regarder ensemble : écran {id}, page partie ailleurs : {vid} en pause à {at:.1} s");
                    self.local(id, vid, at, true, short, me);
                }
            }
            return;
        };
        if Some(&v) != shared.as_ref() {
            // Une autre vidéo ouverte sur cette page (clic, vidéo suivante, `/yt`).
            if before.as_ref() != Some(&v) {
                self.local(id, v, page.t, page.paused, page.short, me);
            }
            return;
        }
        // Même vidéo : lancée, mise en pause, avancée ou reculée ?
        let Some(prev) = prev else {
            return;
        };
        let p = &prev.page;
        if before.as_ref() != Some(&v) || p.ad || p.t < 0.0 || page.t < 0.0 {
            return;
        }
        let elapsed = if p.paused { 0.0 } else { prev.age };
        let jumped = page.t > p.t + elapsed + JUMP_FWD_S || page.t < p.t - JUMP_BACK_S;
        if page.paused != p.paused || jumped {
            self.local(id, v, page.t, page.paused, page.short, me);
        }
    }

    /// Geste du joueur sur la page de l'écran `id` : nouvelle vidéo partagée.
    fn local(&mut self, id: u32, video: String, t: f32, paused: bool, short: bool, me: &str) {
        let seq = self
            .tracks
            .get(&id)
            .map_or(1, |t| t.video.seq.saturating_add(1))
            .min(SEQ_MAX - 1);
        let by: String = me.chars().take(32).collect();
        // (v291) Une autre vidéo ouverte sur la page : celle d'avant passe dans les vues ;
        // la playlist reste (une page Google ou un autre site n'y touchent pas).
        let avant = self.tracks.get(&id).map(|t| &t.video);
        let mut lus = avant.map(|v| v.lus.clone()).unwrap_or_default();
        if let Some(v) = avant.filter(|v| v.id != video) {
            lus.push(v.id.clone());
        }
        let mut video = Video {
            id: video,
            t: t.clamp(0.0, MAX_T),
            paused,
            seq,
            by: by.clone(),
            short,
            queue: avant.map(|v| v.queue.clone()).unwrap_or_default(),
            lus,
        };
        video.rogne();
        self.tracks.insert(
            id,
            Track {
                applied: Some((seq, by)),
                applied_video: Some(video.id.clone()),
                video,
                age: 0.0,
                settle: 0.0,
                corrected: CORRECT_S,
                guess: t < 0.0,
                fine: false,
                seul: true,
            },
        );
        self.changed.push(id);
    }

    /// (v253) Nouvelle vidéo partagée de l'écran `id` donnée par `me` (rang suivant) : la
    /// page sera amenée à elle par `update`, comme pour une vidéo reçue.
    fn give(&mut self, id: u32, video: Video) {
        self.tracks.insert(
            id,
            Track {
                video,
                age: 0.0,
                applied: None,
                applied_video: None,
                settle: 0.0,
                corrected: CORRECT_S,
                guess: false,
                fine: false,
                seul: true,
            },
        );
        self.changed.push(id);
    }

    fn next_seq(&self, id: u32) -> u32 {
        self.tracks
            .get(&id)
            .map_or(1, |t| t.video.seq.saturating_add(1))
            .min(SEQ_MAX - 1)
    }

    /// (v253) La vidéo de l'écran `id` est finie : la première de la file la remplace.
    fn advance(&mut self, id: u32, me: &str) {
        let Some(t) = self.tracks.get(&id) else {
            return;
        };
        let mut queue = t.video.queue.clone();
        if queue.is_empty() {
            return;
        }
        let next = queue.remove(0);
        log::info!("regarder ensemble : écran {id}, vidéo finie, suivante de la file : {next}");
        // (v291) La finie reste dans la playlist, avec les vues.
        let mut lus = t.video.lus.clone();
        lus.push(t.video.id.clone());
        let mut video = Video {
            id: next,
            t: 0.0,
            paused: false,
            seq: self.next_seq(id),
            by: me.chars().take(32).collect(),
            short: false,
            queue,
            lus,
        };
        video.rogne();
        self.give(id, video);
    }

    /// (v253) Un ordre de la page de l'écran `id` (JSON du navigateur). Rend ce qu'il faut
    /// faire à la page :
    /// - `{"clic":vidéo}` (une vidéo cliquée sur le site de YouTube) : si une vidéo est en
    ///   cours sur l'écran, la cliquée va au bout de sa file et la page revient à celle en
    ///   cours ; sinon elle s'ouvre ;
    /// - `{"retire":rang,"v":vidéo}` (la croix dans la file) : retirée de la file.
    ///
    /// (v291) Les rangs sont ceux de la playlist entière (vues, en cours, à suivre) :
    /// - `{"joue":rang,"v":vidéo}` (clic sur une vidéo de la playlist) : elle se lance pour
    ///   tous ; celles d'avant comptent comme vues, celles d'après sont à suivre ;
    /// - `{"monte":rang,"v":vidéo}`, `{"descend":rang,"v":vidéo}` (les flèches) : échangée
    ///   avec sa voisine, vues comprises ; celle en cours continue ;
    /// - `{"retire":rang,"v":vidéo}` : retirée (pas celle en cours).
    ///
    /// (v257) `cote` : l'ordre vient de la recherche à côté du lecteur (un autre onglet) ; la
    /// page du lecteur n'a pas bougé, rien n'est rouvert quand la vidéo va dans la file.
    pub fn order(&mut self, id: u32, json: &str, me: &str, cote: bool) -> Option<Act> {
        #[derive(Deserialize)]
        struct Ordre {
            #[serde(default)]
            clic: Option<String>,
            #[serde(default)]
            retire: Option<usize>,
            #[serde(default)]
            joue: Option<usize>,
            #[serde(default)]
            monte: Option<usize>,
            #[serde(default)]
            descend: Option<usize>,
            #[serde(default)]
            reprend: Option<usize>,
            #[serde(default)]
            v: Option<String>,
        }
        let o: Ordre = serde_json::from_str(json).ok()?;
        if let Some(vid) = o.clic.filter(|v| valid_id(v)) {
            if cote {
                return self.file_cote(id, vid, me);
            }
            let en_cours = self.tracks.get(&id).filter(|t| {
                let fini = self.pages.get(&id).is_some_and(|s| {
                    s.page.ended && s.page.video.as_deref() == Some(t.video.id.as_str())
                });
                !t.video.short && !fini
            });
            let Some(t) = en_cours else {
                return Some(Act::Open(id, watch_url(&vid, 0.0)));
            };
            let retour = (!cote).then(|| Act::Open(id, watch_url(&t.video.id, t.now())));
            if t.video.queue.len() >= MAX_FILE || t.video.id == vid {
                return retour;
            }
            let mut video = t.at_now();
            video.queue.push(vid.clone());
            video.seq = self.next_seq(id);
            video.by = me.chars().take(32).collect();
            log::info!("regarder ensemble : écran {id}, {vid} ajoutée à la file ({} en tout)", video.queue.len());
            self.put(id, video);
            return retour;
        }
        let geste = o
            .joue
            .map(|i| (Geste::Joue, i))
            .or(o.monte.map(|i| (Geste::Monte, i)))
            .or(o.descend.map(|i| (Geste::Descend, i)))
            .or(o.retire.map(|i| (Geste::Retire, i)))
            .or(o.reprend.map(|i| (Geste::Reprend, i)));
        let (Some((g, i)), Some(vid)) = (geste, o.v) else {
            return None;
        };
        self.geste(id, g, i, &vid, me)
    }

    /// (v291) Un geste sur la playlist de l'écran `id` : la vidéo au rang `i` (de la
    /// playlist entière) doit être `vid`, sinon rien (la playlist a changé entre-temps).
    fn geste(&mut self, id: u32, g: Geste, i: usize, vid: &str, me: &str) -> Option<Act> {
        let t = self.tracks.get(&id)?;
        let (mut l, mut pos) = t.video.liste();
        if l.get(i).map(String::as_str) != Some(vid) {
            return None;
        }
        let mut video = t.at_now();
        video.seq = self.next_seq(id);
        video.by = me.chars().take(32).collect();
        match g {
            Geste::Reprend => {
                if i != pos {
                    return None;
                }
                // Celle en cours, en pause (la page est partie ailleurs, ou mise en pause) :
                // elle reprend pour tous là où elle en était, et la page y revient. Pas en
                // pause : la page seule y revient, au temps de tous.
                if !video.paused {
                    return Some(Act::Open(id, open_url(&video, t.now())));
                }
                video.paused = false;
                log::info!("regarder ensemble : écran {id}, {vid} reprise à {:.1} s", video.t);
                self.give(id, video);
                return None;
            }
            Geste::Joue => {
                if i == pos {
                    return None;
                }
                video.set_liste(l, i);
                video.t = 0.0;
                video.paused = false;
                video.short = false;
                log::info!("regarder ensemble : écran {id}, {vid} lancée depuis la playlist");
                self.give(id, video);
                return None;
            }
            Geste::Monte | Geste::Descend => {
                let j = if g == Geste::Monte { i.checked_sub(1) } else { Some(i + 1) };
                let j = j.filter(|&j| j < l.len())?;
                l.swap(i, j);
                if pos == i {
                    pos = j;
                } else if pos == j {
                    pos = i;
                }
            }
            Geste::Retire => {
                if i == pos {
                    return None;
                }
                l.remove(i);
                if i < pos {
                    pos -= 1;
                }
                log::info!("regarder ensemble : écran {id}, {vid} retirée de la playlist");
            }
        }
        video.set_liste(l, pos);
        self.put(id, video);
        None
    }

    /// (v259) Une vidéo cliquée dans la recherche ouverte depuis la file : **toujours**
    /// ajoutée au bout de la file, jamais lancée (demande du propriétaire). Rien encore de
    /// partagé sur l'écran (vidéo pas encore lancée, ou attendant le regard) : la vidéo que
    /// montre la page devient celle de l'écran, avec cette file. Seule une page sans vidéo du
    /// tout (rien à quoi ajouter) ouvre la cliquée.
    fn file_cote(&mut self, id: u32, vid: String, me: &str) -> Option<Act> {
        if !self.tracks.contains_key(&id) {
            let page = self.pages.get(&id).map(|s| s.page.clone());
            let Some((cur, p)) = page.and_then(|p| Some((p.video.clone()?, p))) else {
                return Some(Act::Open(id, watch_url(&vid, 0.0)));
            };
            self.local(id, cur, p.t.max(0.0), p.paused, p.short, me);
        }
        let mut video = self.tracks.get(&id)?.at_now();
        if video.queue.len() >= MAX_FILE || video.queue.last() == Some(&vid) {
            return None;
        }
        video.queue.push(vid.clone());
        video.seq = self.next_seq(id);
        video.by = me.chars().take(32).collect();
        log::info!(
            "regarder ensemble : écran {id}, {vid} ajoutée à la file depuis la recherche ({} en tout)",
            video.queue.len()
        );
        self.put(id, video);
        None
    }

    /// (v289) La playlist de l'écran `id`, que son volet montre : (v291) les vues, celle en
    /// cours, celles à suivre, et si celle en cours est en pause. `None` : rien de partagé
    /// sur l'écran.
    pub fn liste(&self, id: u32) -> Option<(&[String], &str, &[String], bool)> {
        self.tracks.get(&id).map(|t| {
            (&t.video.lus[..], t.video.id.as_str(), &t.video.queue[..], t.video.paused)
        })
    }

    /// (v253) La même vidéo, au même point, avec une autre file : rien à faire à la page
    /// que de lui redonner sa file.
    fn put(&mut self, id: u32, video: Video) {
        if let Some(t) = self.tracks.get_mut(&id) {
            t.applied = Some((video.seq, video.by.clone()));
            t.video = video;
            t.age = 0.0;
            self.changed.push(id);
        }
    }

    /// Message reçu de `from` (dans le canal de la voix). `false` : ce n'est pas un des
    /// nôtres (il est pour la voix). `from_pos` : où est celui qui l'envoie, si on le voit.
    pub fn receive(
        &mut self,
        from: &str,
        data: &[u8],
        from_pos: Option<Vec3>,
        local: &[Placed],
    ) -> bool {
        let Some(body) = data.strip_prefix(&MAGIC[..]) else {
            return false;
        };
        let Some(msg) = minelib_net::game::decode::<Msg>(body) else {
            return true;
        };
        let local_ids: HashSet<u32> = local.iter().map(Placed::id).collect();
        match msg {
            Msg::Screens(list) => {
                // On ne croit un écran que près de celui qui l'annonce.
                let Some(pos) = from_pos else {
                    return true;
                };
                let mut screens: Vec<Placed> = Vec::new();
                for s in list.into_iter().take(MAX_PER_MSG) {
                    let p = Placed {
                        block: s.block,
                        normal: s.normal,
                        facing: s.facing,
                        size: s.size,
                    };
                    if !valid_place(&p)
                        || (center(&p) - pos).length() > ACCEPT_RANGE
                        || local_ids.contains(&p.id())
                        || screens.iter().any(|q| q.id() == p.id())
                    {
                        continue;
                    }
                    if let Some(v) = s.video {
                        self.offer(p.id(), v, from);
                    }
                    screens.push(p);
                }
                self.remote.insert(
                    from.to_lowercase(),
                    Remote {
                        owner: from.to_string(),
                        screens,
                        age: 0.0,
                    },
                );
            }
            Msg::Video { screen, video } => {
                let known = local_ids.contains(&screen)
                    || self
                        .remote
                        .values()
                        .any(|r| r.screens.iter().any(|s| s.id() == screen));
                if known {
                    self.offer(screen, video, from);
                }
            }
        }
        true
    }

    /// Vidéo de l'écran `id` reçue de `from` : gardée si elle est plus récente ; s'il a
    /// plus ancien que nous, on lui répond avec la nôtre.
    fn offer(&mut self, id: u32, v: Video, from: &str) {
        if !v.valid() {
            return;
        }
        match self.tracks.get_mut(&id) {
            None => {
                self.tracks.insert(
                    id,
                    Track {
                        video: v,
                        age: 0.0,
                        applied: None,
                        applied_video: None,
                        settle: 0.0,
                        corrected: CORRECT_S,
                        guess: false,
                        fine: false,
                        seul: true,
                    },
                );
            }
            Some(t) if v.newer_than(&t.video) => {
                if v.seq > t.video.seq.saturating_add(SEQ_JUMP) {
                    return;
                }
                t.video = v;
                t.age = 0.0;
                t.guess = false;
            }
            Some(t) => {
                if t.video.newer_than(&v) && !t.guess && t.corrected >= CORRECT_S {
                    t.corrected = 0.0;
                    self.corrections.push((from.to_string(), id));
                }
            }
        }
    }

    /// Avance de `dt` : écrans des autres oubliés, messages à envoyer (voir
    /// [`Party::take_out`]) et ce qu'il faut faire aux pages pour les amener à la vidéo
    /// partagée. `local` : ses écrans ; `players` : les joueurs vus (pseudo, position).
    pub fn update(
        &mut self,
        dt: f32,
        local: &[Placed],
        players: &[(String, Vec3)],
    ) -> Vec<Act> {
        for t in self.tracks.values_mut() {
            t.age += dt;
            t.settle -= dt;
            t.corrected += dt;
        }
        for p in self.pages.values_mut() {
            p.age += dt;
        }
        self.remote.retain(|_, r| {
            r.age += dt;
            r.age < EXPIRE_S
        });
        // Écrans montrés : les siens, puis ceux des autres.
        let mut shown: HashMap<u32, (Placed, Option<String>)> =
            local.iter().map(|s| (s.id(), (*s, None))).collect();
        for (s, owner) in self.screens(local) {
            shown.insert(s.id(), (s, Some(owner)));
        }
        self.tracks.retain(|id, _| shown.contains_key(id));
        self.pages.retain(|id, _| shown.contains_key(id));

        // Vidéos changées ici : aux joueurs près de l'écran, et à son propriétaire.
        for id in std::mem::take(&mut self.changed) {
            let (Some(t), Some((s, owner))) = (self.tracks.get(&id), shown.get(&id)) else {
                continue;
            };
            let mut to = near(players, center(s), RANGE);
            if let Some(o) = owner {
                if !to.iter().any(|p| p.eq_ignore_ascii_case(o)) {
                    to.push(o.clone());
                }
            }
            let msg = Msg::Video {
                screen: id,
                video: t.at_now(),
            };
            self.send(to, &msg);
        }
        for (who, id) in std::mem::take(&mut self.corrections) {
            if let Some(t) = self.tracks.get(&id) {
                let msg = Msg::Video {
                    screen: id,
                    video: t.at_now(),
                };
                self.send(vec![who], &msg);
            }
        }
        self.announce_in -= dt;
        if self.announce_in <= 0.0 || self.dirty {
            self.announce_in = ANNOUNCE_S;
            self.dirty = false;
            self.announce(local, players);
        }

        // Pages amenées à la vidéo partagée.
        let mut acts = Vec::new();
        for (&id, t) in self.tracks.iter_mut() {
            let page = self.pages.get(&id).map(|s| &s.page);
            // (v253) La page du lecteur montrait la file de l'écran ; (v289) c'est le volet
            // de l'écran qui la montre, sur toutes les pages (voir `Party::file`).
            // Depuis combien de temps la page a donné ce temps.
            let page_age = self.pages.get(&id).map_or(0.0, |s| s.age);
            // (v213) Personne d'autre près de l'écran : rien à garder ensemble (voir plus bas).
            let seul = shown
                .get(&id)
                .is_none_or(|(s, _)| near(players, center(s), RANGE).is_empty());
            t.seul = seul;
            let key = (t.video.seq, t.video.by.clone());
            let on_it = page.filter(|p| p.video.as_deref() == Some(t.video.id.as_str()));
            if t.applied.as_ref() != Some(&key) {
                t.applied = Some(key);
                if t.applied_video.as_deref() != Some(t.video.id.as_str()) {
                    // Nouvelle vidéo : ouverte, même si la page était ailleurs.
                    t.applied_video = Some(t.video.id.clone());
                    log::info!(
                        "regarder ensemble : écran {id}, vidéo {} donnée par {} ouverte à {:.1} s",
                        t.video.id,
                        t.video.by,
                        t.now()
                    );
                    acts.push(Act::Open(id, open_url(&t.video, t.now())));
                    t.settle = SETTLE_OPEN_S;
                    t.fine = true;
                } else if let Some(p) = on_it {
                    // Même vidéo : temps et pause (une page partie ailleurs y reste).
                    let seek = (p.t < 0.0 || (p.t - t.now()).abs() > FINE_S).then(|| t.now());
                    log::info!(
                        "regarder ensemble : écran {id}, geste de {} ({}{})",
                        t.video.by,
                        if t.video.paused { "pause" } else { "lecture" },
                        seek.map_or(String::new(), |s| format!(", page à {:.1} s amenée à {s:.1} s", p.t))
                    );
                    acts.push(Act::Eval(id, control_js(seek, t.video.paused)));
                    t.settle = SETTLE_EVAL_S;
                }
                continue;
            }
            // Page qui s'écarte (chargement, pub, pause oubliée) : recalée.
            let Some(p) = on_it else {
                continue;
            };
            if t.settle > 0.0 || t.guess || p.ad || p.t < 0.0 {
                continue;
            }
            // (v213) Personne d'autre près de l'écran : rien à garder ensemble. La vidéo
            // partagée suit alors la page (une page qui a bloqué un instant n'est jamais
            // avancée d'un saut, ce qui s'entendait comme une petite coupure) ; un joueur
            // qui arrive reçoit le temps où en est vraiment la page.
            if seul {
                t.video.t = p.t.clamp(0.0, MAX_T);
                t.video.paused = p.paused;
                t.age = page_age;
                t.fine = false;
                continue;
            }
            let off = (p.t - t.now()).abs();
            let limit = if t.fine { FINE_S } else { DRIFT_S };
            t.fine = false;
            if p.paused != t.video.paused || off > limit {
                let seek = (off > FINE_S).then(|| t.now());
                // (v213) Chaque recalage est écrit : une coupure entendue se retrouve ici
                // si elle vient du jeu.
                log::info!(
                    "regarder ensemble : écran {id} recalé (page à {:.1} s{}, vidéo partagée à {:.1} s{}){}",
                    p.t,
                    if p.paused { " en pause" } else { "" },
                    t.now(),
                    if t.video.paused { " en pause" } else { "" },
                    if seek.is_some() { ", saut" } else { ", sans saut" }
                );
                acts.push(Act::Eval(id, control_js(seek, t.video.paused)));
                t.settle = SETTLE_EVAL_S;
            }
        }
        acts
    }

    /// Ses écrans, à chaque joueur qui en a près de lui (les plus proches d'abord) ; à qui
    /// en avait et n'en a plus, la liste vide.
    fn announce(&mut self, local: &[Placed], players: &[(String, Vec3)]) {
        let mut groups: HashMap<Vec<u32>, Vec<String>> = HashMap::new();
        let mut told = HashMap::new();
        for (who, pos) in players {
            let mut close: Vec<(f32, u32)> = local
                .iter()
                .map(|s| ((center(s) - *pos).length(), s.id()))
                .filter(|(d, _)| *d <= RANGE)
                .collect();
            close.sort_by(|a, b| a.0.total_cmp(&b.0));
            close.truncate(MAX_PER_MSG);
            let mut ids: Vec<u32> = close.into_iter().map(|c| c.1).collect();
            ids.sort_unstable();
            let key = who.to_lowercase();
            if ids.is_empty() && self.told.get(&key).is_none_or(Vec::is_empty) {
                continue;
            }
            told.insert(key, ids.clone());
            groups.entry(ids).or_default().push(who.clone());
        }
        self.told = told;
        for (ids, to) in groups {
            let list: Vec<Shared> = ids
                .iter()
                .filter_map(|id| local.iter().find(|s| s.id() == *id))
                .map(|s| Shared {
                    block: s.block,
                    normal: s.normal,
                    facing: s.facing,
                    size: s.size,
                    video: self.tracks.get(&s.id()).map(Track::at_now),
                })
                .collect();
            let msg = Msg::Screens(list);
            for chunk in to.chunks(MAX_TO) {
                self.send(chunk.to_vec(), &msg);
            }
        }
    }

    fn send(&mut self, to: Vec<String>, msg: &Msg) {
        if to.is_empty() {
            return;
        }
        let mut data = MAGIC.to_vec();
        data.extend(minelib_net::game::encode(msg));
        if data.len() <= MAX_BYTES {
            for chunk in to.chunks(MAX_TO) {
                self.out.push((chunk.to_vec(), data.clone()));
            }
        }
    }

    /// Messages à envoyer : (destinataires, contenu), par le canal de la voix.
    pub fn take_out(&mut self) -> Vec<(Vec<String>, Vec<u8>)> {
        std::mem::take(&mut self.out)
    }
}

/// (v291) Un geste sur la playlist d'un écran (voir [`Party::order`]).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Geste {
    Joue,
    Monte,
    Descend,
    Retire,
    Reprend,
}

/// Joueurs à moins de `range` de `p`.
fn near(players: &[(String, Vec3)], p: Vec3, range: f32) -> Vec<String> {
    players
        .iter()
        .filter(|(_, q)| (*q - p).length() <= range)
        .map(|(n, _)| n.clone())
        .collect()
}

fn center(s: &Placed) -> Vec3 {
    Vec3::from(s.anchor().center)
}

/// Un écran possible : posé sur une face (un axe), tourné à l'horizontale, d'une taille
/// connue, dans le monde.
fn valid_place(p: &Placed) -> bool {
    let unit = |v: [i32; 3]| v.iter().map(|c| c.abs()).sum::<i32>() == 1;
    usize::from(p.size) < crate::screen::SIZES.len()
        && unit(p.normal)
        && unit(p.facing)
        && p.facing[1] == 0
        && (p.normal[1] != 0 || p.facing == p.normal)
        && p.block.iter().all(|c| c.abs() < 30_000_000)
}

/// Numéro de vidéo YouTube : 11 caractères `A-Z a-z 0-9 - _`.
pub fn valid_id(id: &str) -> bool {
    // (v265) Vimeo : `vm:` et son numéro ; Dailymotion : `dm:` et son identifiant.
    if let Some(n) = id.strip_prefix("vm:") {
        return (1..=12).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_digit());
    }
    if let Some(x) = id.strip_prefix("dm:") {
        return (5..=12).contains(&x.len()) && x.bytes().all(|b| b.is_ascii_alphanumeric());
    }
    id.len() == 11
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// (v210) La page du lecteur officiel de YouTube sur les écrans, servie par le navigateur
/// du jeu (`crates/browser/src/lecteur.html`, même adresse que `filtre::LECTEUR`). Le site
/// complet de YouTube ne sert plus qu'à chercher : ses vidéos s'ouvrent là.
pub const LECTEUR: &str = "https://viendax.github.io/minelib/lecteur";

/// Vidéo YouTube d'une adresse (`youtube.com/watch?v=…`, `/shorts/…`, `/embed/…`,
/// `/live/…`, `youtu.be/…`, et la page du lecteur), s'il y en a une.
pub fn video_of(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let rest = rest.split('#').next()?;
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let (path, query) = path.split_once('?').unwrap_or((path, ""));
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let id = match host.to_ascii_lowercase().as_str() {
        "viendax.github.io" if path == "minelib/lecteur" => {
            let v = query.split('&').find_map(|kv| kv.strip_prefix("v="))?;
            // (v265) `vm:…`, `dm:…` : le deux-points peut arriver encodé.
            let v = v.replace("%3A", ":").replace("%3a", ":");
            return valid_id(&v).then_some(v);
        }
        // (v265) Vimeo et Dailymotion.
        "vimeo.com" | "www.vimeo.com" | "player.vimeo.com" => {
            let n = match segs.as_slice() {
                ["video", n, ..] if host.starts_with("player.") => *n,
                [n, ..] if n.bytes().all(|b| b.is_ascii_digit()) => *n,
                ["channels", _, n, ..] => *n,
                ["groups", _, "videos", n, ..] => *n,
                _ => return None,
            };
            let v = format!("vm:{n}");
            return valid_id(&v).then_some(v);
        }
        "dailymotion.com" | "www.dailymotion.com" | "geo.dailymotion.com" | "dai.ly" => {
            let x = match segs.as_slice() {
                [x] if host == "dai.ly" => *x,
                ["video", x, ..] => *x,
                _ => return None,
            };
            let v = format!("dm:{x}");
            return valid_id(&v).then_some(v);
        }
        "youtu.be" => path.split('/').next()?,
        "www.youtube.com" | "youtube.com" | "m.youtube.com" => {
            if path == "watch" {
                query.split('&').find_map(|kv| kv.strip_prefix("v="))?
            } else {
                let r = ["shorts/", "embed/", "live/"]
                    .iter()
                    .find_map(|p| path.strip_prefix(p))?;
                r.split('/').next()?
            }
        }
        _ => return None,
    };
    valid_id(id).then(|| id.to_string())
}

/// Adresse qui ouvre la vidéo `id` au temps `t` : (v210) la page du lecteur.
pub fn watch_url(id: &str, t: f32) -> String {
    format!("{LECTEUR}?v={id}&t={}", t.max(0.0) as u32)
}

/// (v246) L'adresse d'un Short sur le site de YouTube (il recommence en boucle : le temps
/// est corrigé ensuite, une fois la page là).
pub fn short_url(id: &str) -> String {
    format!("https://www.youtube.com/shorts/{id}")
}

/// (v246) L'adresse qui ouvre une vidéo partagée : le lecteur, ou le site pour un Short.
fn open_url(v: &Video, t: f32) -> String {
    if v.short {
        short_url(&v.id)
    } else {
        watch_url(&v.id, t)
    }
}

/// (v246) L'adresse est-elle un Short du site de YouTube ?
pub fn is_short(url: &str) -> bool {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or("");
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    matches!(
        host.to_ascii_lowercase().as_str(),
        "www.youtube.com" | "youtube.com" | "m.youtube.com"
    ) && path.starts_with("shorts/")
        && video_of(url).is_some()
}

/// (v210) L'adresse est-elle la page du lecteur ?
pub fn is_player(url: &str) -> bool {
    url.strip_prefix(LECTEUR)
        .is_some_and(|r| r.is_empty() || r.starts_with('?') || r.starts_with('#'))
}

/// Script qui met la vidéo de la page au temps `seek` (s'il y en a un) et en pause ou en
/// lecture (v210 : la page du lecteur, qui ne lance la lecture que si elle en a le droit ;
/// sinon le lecteur du site de YouTube, ou la balise vidéo).
pub fn control_js(seek: Option<f32>, paused: bool) -> String {
    let seek = seek.map_or(String::new(), |t| {
        format!("if(p&&p.seekTo)p.seekTo({t:.1},true);else if(v)v.currentTime={t:.1};")
    });
    let play = if paused {
        "if(p&&p.pauseVideo)p.pauseVideo();else if(v)v.pause();"
    } else {
        "if(p&&p.playVideo)p.playVideo();else if(v)v.play();"
    };
    format!(
        "(()=>{{const p=window.__mlb||document.getElementById(location.pathname.startsWith('/shorts/')?'shorts-player':'movie_player');\
         const v=(p&&p.querySelector&&p.querySelector('video'))||document.querySelector('video');{seek}{play}}})()"
    )
}

/// État rapporté par une page (`{"u":…,"t":…,"p":…,"a":…,"i":…}`). Le temps n'est sûr
/// que si le lecteur joue la vidéo de l'adresse (`i`, quand il le dit).
pub fn parse_page(json: &str) -> Option<Page> {
    #[derive(Deserialize)]
    struct Raw {
        u: String,
        t: f64,
        p: bool,
        #[serde(default)]
        a: bool,
        #[serde(default)]
        i: String,
        #[serde(default)]
        e: bool,
        #[serde(default)]
        f: Option<String>,
    }
    let r: Raw = serde_json::from_str(json).ok()?;
    let video = video_of(&r.u);
    let mut t = r.t as f32;
    if !t.is_finite() || t < 0.0 || video.is_none() {
        t = -1.0;
    }
    if !r.i.is_empty() && video.as_deref() != Some(r.i.as_str()) {
        t = -1.0;
    }
    let short = is_short(&r.u);
    Some(Page {
        video,
        t,
        paused: r.p,
        ad: r.a,
        short,
        ended: r.e,
        file: r.f,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "Alice";
    const B: &str = "Bob";
    const X: &str = "dQw4w9WgXcQ";
    const Y: &str = "9bZkp7q19f0";

    /// (v265) Vimeo et Dailymotion : `vm:`, `dm:`, dans le lecteur comme sur leurs sites.
    #[test]
    fn videos_de_vimeo_et_dailymotion() {
        assert!(valid_id("vm:76979871") && valid_id("dm:x8abcde") && valid_id(X));
        assert!(!valid_id("vm:12a") && !valid_id("dm:x") && !valid_id("vm:") && !valid_id("yt:abc"));
        assert_eq!(video_of("https://vimeo.com/76979871"), Some("vm:76979871".into()));
        assert_eq!(video_of("https://vimeo.com/channels/staffpicks/76979871"), Some("vm:76979871".into()));
        assert_eq!(video_of("https://player.vimeo.com/video/76979871?h=ab"), Some("vm:76979871".into()));
        assert_eq!(video_of("https://vimeo.com/search?q=chat"), None);
        assert_eq!(video_of("https://www.dailymotion.com/video/x8abcde"), Some("dm:x8abcde".into()));
        assert_eq!(video_of("https://dai.ly/x8abcde"), Some("dm:x8abcde".into()));
        assert_eq!(video_of("https://www.dailymotion.com/fr"), None);
        let l = watch_url("dm:x8abcde", 12.0);
        assert_eq!(video_of(&l), Some("dm:x8abcde".into()));
        assert_eq!(video_of(&l.replace(':', "%3A").replacen("https%3A", "https:", 1)), Some("dm:x8abcde".into()));
        assert!(is_player(&watch_url("vm:76979871", 0.0)));
    }

    #[test]
    fn numero_de_video_des_adresses_youtube() {
        let ok = |u: &str| video_of(u);
        assert_eq!(ok("https://www.youtube.com/watch?v=dQw4w9WgXcQ"), Some(X.into()));
        assert_eq!(
            ok("https://www.youtube.com/watch?app=desktop&v=dQw4w9WgXcQ&t=42s#x"),
            Some(X.into())
        );
        assert_eq!(ok("https://youtu.be/dQw4w9WgXcQ?si=abc"), Some(X.into()));
        assert_eq!(ok("https://m.youtube.com/shorts/dQw4w9WgXcQ"), Some(X.into()));
        assert_eq!(ok("https://www.youtube.com/embed/dQw4w9WgXcQ"), Some(X.into()));
        assert_eq!(ok("https://www.youtube.com/"), None);
        assert_eq!(ok("https://www.youtube.com/results?search_query=chat"), None);
        assert_eq!(ok("https://www.youtube.com/watch?v=court"), None);
        assert_eq!(ok("https://evil.example/watch?v=dQw4w9WgXcQ"), None);
        assert_eq!(ok("https://youtube.com.evil.example/watch?v=dQw4w9WgXcQ"), None);
        assert_eq!(ok("https://www.youtube.com/watch?v=dQw4w9WgXc<"), None);
        assert_eq!(watch_url(X, 61.9), format!("{LECTEUR}?v={X}&t=61"));
        assert_eq!(ok(&watch_url(X, 61.9)), Some(X.into()));
        assert!(is_player(&watch_url(X, 0.0)) && !is_player("https://viendax.github.io/"));
        assert_eq!(ok("https://viendax.github.io/minelib/autre?v=dQw4w9WgXcQ"), None);
    }

    #[test]
    fn etat_de_la_page() {
        let p = parse_page(&format!(
            r#"{{"u":"https://www.youtube.com/watch?v={X}","t":12.5,"p":false,"a":false,"i":"{X}"}}"#
        ))
        .unwrap();
        assert_eq!(
            p,
            Page {
                video: Some(X.into()),
                t: 12.5,
                paused: false,
                ad: false,
                short: false,
                ended: false,
                file: None,
            }
        );
        // (v246) Un Short du site.
        let p = parse_page(&format!(
            r#"{{"u":"https://www.youtube.com/shorts/{X}","t":3,"p":false,"a":false,"i":"{X}"}}"#
        ))
        .unwrap();
        assert!(p.short && p.video.as_deref() == Some(X));
        assert!(is_short(&short_url(X)) && !is_short(&watch_url(X, 0.0)));
        assert!(!is_short("https://www.youtube.com/shorts/court"));
        // Le lecteur joue encore l'ancienne vidéo : temps pas sûr.
        let p = parse_page(&format!(
            r#"{{"u":"https://www.youtube.com/watch?v={X}","t":80,"p":false,"i":"{Y}"}}"#
        ))
        .unwrap();
        assert_eq!(p.t, -1.0);
        // Ancien navigateur (ni `a` ni `i`).
        let p = parse_page(r#"{"u":"https://www.youtube.com/","t":-1,"p":true}"#).unwrap();
        assert_eq!((p.video, p.t, p.paused, p.ad), (None, -1.0, true, false));
        assert!(parse_page("pas du json").is_none());
    }

    #[test]
    fn script_de_controle() {
        let js = control_js(Some(12.0), true);
        assert!(js.contains("seekTo(12.0,true)") && js.contains("pauseVideo"));
        let js = control_js(None, false);
        assert!(!js.contains("seekTo") && js.contains("playVideo"));
        assert!(!js.contains('\n'));
        assert!(js.contains("window.__mlb||"));
    }

    /// Une page de navigateur simulée : ouvre les vidéos, avance, obéit aux scripts.
    struct FakePage {
        url: String,
        t: f32,
        paused: bool,
        ad: f32,
        stall: f32,
        loading: Option<(f32, String, f32)>,
        report_in: f32,
        last: String,
        ended: bool,
    }

    impl FakePage {
        fn new() -> Self {
            Self {
                url: "https://www.youtube.com/".into(),
                t: -1.0,
                paused: true,
                ad: 0.0,
                stall: 0.0,
                loading: None,
                report_in: 0.0,
                last: String::new(),
                ended: false,
            }
        }

        fn act(&mut self, a: &Act) {
            match a {
                Act::Open(_, url) => {
                    let t = url
                        .split("&t=")
                        .nth(1)
                        .and_then(|s| s.trim_end_matches('s').parse().ok())
                        .unwrap_or(0.0);
                    self.loading = Some((0.8, url.clone(), t));
                    self.ended = false;
                }
                Act::Eval(_, js) => {
                    if let Some(s) = js.split("seekTo(").nth(1) {
                        self.t = s.split(',').next().unwrap().parse().unwrap();
                    }
                    self.paused = js.contains("pauseVideo()");
                }
            }
        }

        /// Avance ; renvoie un rapport toutes les 0,5 s s'il a changé.
        fn step(&mut self, dt: f32) -> Option<String> {
            if let Some((left, url, t)) = &mut self.loading {
                *left -= dt;
                if *left <= 0.0 {
                    self.url = url.clone();
                    self.t = *t;
                    self.paused = false;
                    self.loading = None;
                }
            } else if self.ad > 0.0 {
                self.ad -= dt;
            } else if self.stall > 0.0 {
                self.stall -= dt;
            } else if !self.paused && self.t >= 0.0 {
                self.t += dt;
            }
            self.report_in -= dt;
            if self.report_in > 0.0 {
                return None;
            }
            self.report_in = 0.5;
            let ad = self.ad > 0.0;
            let t = if ad { 3.0 } else { self.t };
            let r = format!(
                r#"{{"u":"{}","t":{:.1},"p":{},"a":{ad},"e":{}}}"#,
                self.url,
                (t * 10.0).round() / 10.0,
                self.paused,
                self.ended
            );
            (r != self.last).then(|| {
                self.last = r.clone();
                r
            })
        }

        fn video(&self) -> Option<String> {
            video_of(&self.url)
        }
    }

    /// Un joueur : sa partie, sa position, ses écrans, la page de chaque écran.
    struct Player {
        name: &'static str,
        pos: Vec3,
        local: Vec<Placed>,
        party: Party,
        pages: HashMap<u32, FakePage>,
    }

    impl Player {
        fn new(name: &'static str, pos: Vec3) -> Self {
            Self {
                name,
                pos,
                local: Vec::new(),
                party: Party::new(),
                pages: HashMap::new(),
            }
        }

        fn shown(&self) -> Vec<u32> {
            let mut ids: Vec<u32> = self.local.iter().map(Placed::id).collect();
            ids.extend(self.party.screens(&self.local).iter().map(|s| s.0.id()));
            ids
        }

        fn page(&mut self, id: u32) -> &mut FakePage {
            self.pages.get_mut(&id).unwrap()
        }
    }

    /// Fait tourner les joueurs `secs` secondes (pas de 50 ms), messages remis tout de suite.
    fn run(players: &mut [Player], secs: f32) {
        let dt = 0.05;
        for _ in 0..(secs / dt).round() as usize {
            let seen: Vec<(String, Vec3)> =
                players.iter().map(|p| (p.name.to_string(), p.pos)).collect();
            let mut mail = Vec::new();
            for p in players.iter_mut() {
                let ids = p.shown();
                p.pages.retain(|id, _| ids.contains(id));
                for id in &ids {
                    p.pages.entry(*id).or_insert_with(FakePage::new);
                }
                for id in ids {
                    if let Some(r) = p.page(id).step(dt) {
                        p.party.observe(id, &r, p.name);
                    }
                }
                let others: Vec<(String, Vec3)> =
                    seen.iter().filter(|s| s.0 != p.name).cloned().collect();
                for a in p.party.update(dt, &p.local, &others) {
                    let (Act::Open(id, _) | Act::Eval(id, _)) = &a;
                    if let Some(page) = p.pages.get_mut(id) {
                        page.act(&a);
                    }
                }
                for (to, data) in p.party.take_out() {
                    mail.push((p.name, p.pos, to, data));
                }
            }
            for (from, pos, to, data) in mail {
                for q in players.iter_mut() {
                    if to.iter().any(|n| n.eq_ignore_ascii_case(q.name)) {
                        let local = q.local.clone();
                        assert!(q.party.receive(from, &data, Some(pos), &local));
                    }
                }
            }
        }
    }

    /// (v246) Alice glisse d'un Short à l'autre sur le site : Bob est amené à chacun, sur
    /// le site aussi (jamais dans le lecteur).
    #[test]
    fn les_shorts_se_suivent_sur_le_site() {
        let s = screen_at(0);
        let id = s.id();
        let mut ps = vec![
            Player::new(A, Vec3::new(0.0, 65.0, 6.0)),
            Player::new(B, Vec3::new(3.0, 65.0, 8.0)),
        ];
        ps[0].local.push(s);
        ps[0].party.screens_changed();
        run(&mut ps, 1.0);
        ps[0].page(id).act(&Act::Open(id, short_url(X)));
        run(&mut ps, 6.0);
        assert_eq!(ps[1].page(id).url, short_url(X));
        // Short suivant.
        ps[0].page(id).act(&Act::Open(id, short_url(Y)));
        run(&mut ps, 6.0);
        assert_eq!(ps[1].page(id).url, short_url(Y));
        assert_eq!(ps[0].page(id).url, short_url(Y));
        // Puis une vidéo ordinaire : le lecteur.
        ps[1].page(id).act(&Act::Open(id, watch_url(X, 0.0)));
        run(&mut ps, 7.0);
        assert!(is_player(&ps[0].page(id).url), "{}", ps[0].page(id).url);
    }

    /// (v253) Bob clique une vidéo pendant celle d'Alice : elle va dans la file de tous, et
    /// passe quand celle d'Alice finit. Une croix l'en retire. Sans vidéo en cours, un clic
    /// l'ouvre.
    #[test]
    fn la_file_d_attente_se_partage_et_avance() {
        let s = screen_at(0);
        let id = s.id();
        let mut ps = vec![
            Player::new(A, Vec3::new(0.0, 65.0, 6.0)),
            Player::new(B, Vec3::new(3.0, 65.0, 8.0)),
        ];
        ps[0].local.push(s);
        ps[0].party.screens_changed();
        run(&mut ps, 1.0);
        // Rien en cours : le clic ouvre la vidéo.
        assert_eq!(
            ps[1].party.order(id, &format!(r#"{{"clic":"{X}"}}"#), B, false),
            Some(Act::Open(id, watch_url(X, 0.0)))
        );
        ps[0].page(id).act(&Act::Open(id, watch_url(X, 0.0)));
        run(&mut ps, 6.0);
        // Bob clique Y : dans la file, et sa page revient à X.
        let a = ps[1].party.order(id, &format!(r#"{{"clic":"{Y}"}}"#), B, false);
        assert!(matches!(&a, Some(Act::Open(_, u)) if u.contains(X)), "{a:?}");
        let z = "aaaaaaaaaaa";
        // (v257) Depuis la recherche à côté : dans la file, la page du lecteur ne bouge pas.
        assert_eq!(ps[1].party.order(id, &format!(r#"{{"clic":"{z}"}}"#), B, true), None);
        run(&mut ps, 2.0);
        for p in &ps {
            assert_eq!(p.party.tracks[&id].video.queue, vec![Y.to_string(), z.to_string()]);
            assert_eq!(p.party.tracks[&id].video.id, X);
        }
        // Une croix chez Alice retire z (le bon rang et le bon numéro seulement ; v291 : rangs
        // de la playlist entière, X en cours au rang 0).
        assert_eq!(ps[0].party.order(id, &format!(r#"{{"retire":1,"v":"{z}"}}"#), A, false), None);
        assert_eq!(ps[0].party.tracks[&id].video.queue.len(), 2);
        ps[0].party.order(id, &format!(r#"{{"retire":0,"v":"{X}"}}"#), A, false);
        assert_eq!(ps[0].party.tracks[&id].video.id, X, "pas celle en cours");
        ps[0].party.order(id, &format!(r#"{{"retire":2,"v":"{z}"}}"#), A, false);
        run(&mut ps, 2.0);
        assert_eq!(ps[1].party.tracks[&id].video.queue, vec![Y.to_string()]);
        // La vidéo d'Alice finit : tout le monde passe à Y, la file se vide.
        ps[0].page(id).ended = true;
        ps[0].page(id).paused = true;
        run(&mut ps, 7.0);
        for p in &mut ps {
            assert_eq!(p.page(id).video().as_deref(), Some(Y));
            assert!(p.party.tracks[&id].video.queue.is_empty());
            // (v291) X reste dans la playlist, avec les vues.
            assert_eq!(p.party.tracks[&id].video.lus, vec![X.to_string()]);
        }
        // (v289) Le volet montre la playlist de l'écran.
        let empty: &[String] = &[];
        assert_eq!(ps[0].party.liste(id), Some((&[X.to_string()][..], Y, empty, false)));
        assert!(ps[0].party.liste(id + 1).is_none());
    }

    /// (v291) La playlist entière : un clic lance une vidéo (vues avant, à suivre après), les
    /// flèches échangent deux voisines, vues comprises, sans arrêter celle en cours ; une page
    /// Google (ou tout autre site) n'y touche pas ; une autre vidéo ouverte à la main passe
    /// celle d'avant dans les vues.
    #[test]
    fn la_playlist_se_lance_se_range_et_reste() {
        let mut p = Party::new();
        let ecran = screen_at(0);
        let id = ecran.id();
        let (a, b, c, d) = ("aaaaaaaaaaa", "bbbbbbbbbbb", "ccccccccccc", "ddddddddddd");
        let page = |v: &str| {
            format!(r#"{{"u":"{}","t":12,"p":false,"a":false,"i":"{v}","e":false}}"#, watch_url(v, 0.0))
        };
        p.observe(id, &page(a), A);
        for v in [b, c, d] {
            p.order(id, &format!(r#"{{"clic":"{v}"}}"#), A, true);
        }
        let liste = |p: &Party| {
            let (l, cur, q, _) = p.liste(id).unwrap();
            (l.to_vec(), cur.to_string(), q.to_vec())
        };
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(liste(&p), (s(&[]), a.to_string(), s(&[b, c, d])));
        // Une page Google, puis un autre site : la playlist ne bouge pas, et la vidéo en
        // cours se met en pause là où elle en était.
        p.update(3.0, &[ecran], &[]);
        assert!(!p.tracks[&id].video.paused);
        let avant = p.tracks[&id].now();
        p.observe(id, r#"{"u":"https://www.google.com/search?q=chat","t":-1,"p":true,"a":false,"i":"","e":false}"#, A);
        p.update(5.0, &[ecran], &[]);
        p.observe(id, r#"{"u":"https://www.twitch.tv/","t":-1,"p":true,"a":false,"i":"","e":false}"#, A);
        assert_eq!(liste(&p), (s(&[]), a.to_string(), s(&[b, c, d])));
        assert!(p.liste(id).unwrap().3, "en pause");
        assert!(close(p.tracks[&id].now(), avant, 0.01), "{} / {avant}", p.tracks[&id].now());
        // Un clic sur elle : elle reprend à cet endroit, et la page y revient.
        let reprend = |i: usize, v: &str| format!(r#"{{"reprend":{i},"v":"{v}"}}"#);
        assert_eq!(p.order(id, &reprend(1, b), A, false), None);
        assert!(p.liste(id).unwrap().3, "pas la bonne : rien");
        p.order(id, &reprend(0, a), A, false);
        assert!(!p.liste(id).unwrap().3);
        let acts = p.update(0.05, &[ecran], &[]);
        assert!(
            acts.iter().any(|x| matches!(x, Act::Open(_, u) if u.contains(a) && u.contains(&format!("t={}", avant as u32)))),
            "{acts:?}"
        );
        // Pas en pause : la page seule y revient, au temps de tous.
        let ici = p.order(id, &reprend(0, a), A, false);
        assert!(matches!(&ici, Some(Act::Open(_, u)) if u.contains(a)), "{ici:?}");
        // Clic sur c (rang 2) : elle se lance ; a et b sont vues, d à suivre.
        let joue = |i: usize, v: &str| format!(r#"{{"joue":{i},"v":"{v}"}}"#);
        assert_eq!(p.order(id, &joue(2, b), A, false), None, "mauvais numéro : rien");
        assert_eq!(liste(&p).1, a);
        p.order(id, &joue(2, c), A, false);
        assert_eq!(liste(&p), (s(&[a, b]), c.to_string(), s(&[d])));
        let acts = p.update(0.05, &[ecran], &[]);
        assert!(acts.iter().any(|x| matches!(x, Act::Open(_, u) if u.contains(c))), "{acts:?}");
        // Monter d au-dessus de c (en cours) : d passe dans les vues, c continue.
        let t = p.tracks[&id].now();
        p.order(id, &format!(r#"{{"monte":3,"v":"{d}"}}"#), A, false);
        assert_eq!(liste(&p), (s(&[a, b, d]), c.to_string(), s(&[])));
        assert!(close(p.tracks[&id].now(), t, 0.01) && !p.tracks[&id].video.paused);
        // Descendre a (une vue) : elle reste vue tant qu'elle est avant c.
        p.order(id, &format!(r#"{{"descend":0,"v":"{a}"}}"#), A, false);
        assert_eq!(liste(&p), (s(&[b, a, d]), c.to_string(), s(&[])));
        // Descendre c (en cours) sous rien : rien ; monter c : d devient à suivre.
        p.order(id, &format!(r#"{{"descend":3,"v":"{c}"}}"#), A, false);
        assert_eq!(liste(&p).1, c);
        p.order(id, &format!(r#"{{"monte":3,"v":"{c}"}}"#), A, false);
        assert_eq!(liste(&p), (s(&[b, a]), c.to_string(), s(&[d])));
        // Relancer une vue (b, rang 0) : tout le reste est à suivre.
        p.order(id, &joue(0, b), A, false);
        assert_eq!(liste(&p), (s(&[]), b.to_string(), s(&[a, c, d])));
        // Une autre vidéo ouverte à la main sur la page : b passe dans les vues.
        p.update(6.0, &[ecran], &[]);
        let x = "xxxxxxxxxxx";
        p.observe(id, &page(b), A);
        p.observe(id, &page(x), A);
        assert_eq!(liste(&p), (s(&[b]), x.to_string(), s(&[a, c, d])));
        // Jamais plus de MAX_LISTE : les plus anciennes vues s'en vont.
        let mut v = p.tracks[&id].video.clone();
        let (l, _) = v.liste();
        let mut grande: Vec<String> = (0..MAX_LISTE + 5).map(|k| format!("{k:011}")).collect();
        grande.extend(l);
        let pos = grande.len() - 4;
        v.set_liste(grande, pos);
        assert_eq!(v.lus.len() + v.queue.len(), MAX_LISTE);
        assert!(v.valid());
        assert_eq!(v.queue, s(&[a, c, d]));
    }

    /// (v259) Depuis la recherche ouverte par la file, un clic ajoute toujours à la file,
    /// même quand rien n'est encore partagé (vidéo qui attend le regard) : jamais lancé.
    #[test]
    fn un_clic_dans_la_recherche_ajoute_toujours_a_la_file() {
        let mut p = Party::new();
        let id = 7;
        let clic = |v: &str| format!(r#"{{"clic":"{v}"}}"#);
        // Pas même une vidéo sur la page : rien à quoi l'ajouter, elle s'ouvre.
        assert_eq!(p.order(id, &clic(Y), A, true), Some(Act::Open(id, watch_url(Y, 0.0))));
        // Le lecteur montre X, pas encore lancée (« en pub » : rien de partagé).
        let etat = format!(
            r#"{{"u":"{}","t":0,"p":true,"a":true,"i":"{X}","e":false,"f":"","o":true,"r":true}}"#,
            watch_url(X, 0.0)
        );
        p.observe(id, &etat, A);
        assert!(p.tracks.get(&id).is_none());
        assert_eq!(p.order(id, &clic(Y), A, true), None, "pas lancée");
        assert_eq!(p.tracks[&id].video.id, X);
        assert_eq!(p.tracks[&id].video.queue, vec![Y.to_string()]);
        // La même deux fois de suite : une seule fois.
        assert_eq!(p.order(id, &clic(Y), A, true), None);
        assert_eq!(p.tracks[&id].video.queue.len(), 1);
        // Vidéo finie : elle va quand même dans la file (la file passe à la suivante).
        p.tracks.get_mut(&id).unwrap().video.queue.clear();
        assert_eq!(p.order(id, &clic(Y), A, true), None);
        assert_eq!(p.tracks[&id].video.queue, vec![Y.to_string()]);
    }

    fn screen_at(x: i32) -> Placed {
        Placed::new([x, 64, 0], [0, 0, 1], Vec3::new(x as f32, 65.0, 6.0), crate::screen::NORMAL)
    }

    fn close(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn on_regarde_ensemble() {
        let s = screen_at(0);
        let id = s.id();
        let mut ps = vec![
            Player::new(A, Vec3::new(0.0, 65.0, 6.0)),
            Player::new(B, Vec3::new(3.0, 65.0, 8.0)),
        ];
        ps[0].local.push(s);
        ps[0].party.screens_changed();
        run(&mut ps, 1.0);
        // Bob voit l'écran d'Alice (et sait à qui il est).
        assert_eq!(ps[1].party.screens(&[]), vec![(s, A.to_string())]);

        // Alice ouvre une vidéo sur son écran : Bob l'ouvre aussi, au même temps.
        ps[0].page(id).act(&Act::Open(id, watch_url(X, 0.0)));
        run(&mut ps, 6.0);
        let (a, b) = (ps[0].page(id).t, ps[1].page(id).t);
        assert_eq!(ps[1].page(id).video().as_deref(), Some(X));
        assert!(!ps[1].page(id).paused);
        assert!(close(a, b, 1.5), "Alice {a}, Bob {b}");

        // Bob met en pause : Alice aussi.
        ps[1].page(id).paused = true;
        run(&mut ps, 2.0);
        assert!(ps[0].page(id).paused, "pause de Bob pas suivie");
        let (a, b) = (ps[0].page(id).t, ps[1].page(id).t);
        assert!(close(a, b, 1.0), "Alice {a}, Bob {b}");

        // Alice relance et avance à 120 s : Bob suit.
        ps[0].page(id).paused = false;
        ps[0].page(id).t = 120.0;
        run(&mut ps, 3.0);
        let (a, b) = (ps[0].page(id).t, ps[1].page(id).t);
        assert!(!ps[1].page(id).paused);
        assert!(a > 120.0 && close(a, b, 1.5), "Alice {a}, Bob {b}");

        // Une pub chez Bob (temps de la pub) : rien ne part, personne ne saute.
        ps[1].page(id).ad = 4.0;
        run(&mut ps, 3.0);
        assert!(ps[0].page(id).t > 124.0, "Alice a suivi la pub de Bob");
        run(&mut ps, 8.0);
        let (a, b) = (ps[0].page(id).t, ps[1].page(id).t);
        assert!(close(a, b, 1.5), "après la pub : Alice {a}, Bob {b}");

        // Bob change de vidéo : Alice aussi.
        ps[1].page(id).act(&Act::Open(id, watch_url(Y, 0.0)));
        run(&mut ps, 7.0);
        assert_eq!(ps[0].page(id).video().as_deref(), Some(Y));
        let (a, b) = (ps[0].page(id).t, ps[1].page(id).t);
        assert!(close(a, b, 1.5), "Alice {a}, Bob {b}");

        // La page de Bob se fige 6 s (chargement) : recalée, sans faire reculer Alice.
        let before = ps[0].page(id).t;
        ps[1].page(id).stall = 6.0;
        run(&mut ps, 9.0);
        let (a, b) = (ps[0].page(id).t, ps[1].page(id).t);
        assert!(a > before + 8.5, "Alice a reculé : {before} puis {a}");
        assert!(close(a, b, 1.5), "retard pas rattrapé : Alice {a}, Bob {b}");

        // Bob s'en va dans les recherches : rien ne change chez Alice (v291 : la vidéo ne se
        // met en pause en partant que pour qui est seul devant l'écran).
        ps[1].page(id).url = "https://www.youtube.com/results?search_query=chat".into();
        run(&mut ps, 2.0);
        assert_eq!(ps[0].page(id).video().as_deref(), Some(Y));
        assert!(!ps[0].page(id).paused);
        assert!(!ps[1].party.tracks[&id].video.paused);

        // Alice retire son écran : il disparaît chez Bob.
        ps[0].local.clear();
        ps[0].party.screens_changed();
        run(&mut ps, 0.5);
        assert!(ps[1].party.screens(&[]).is_empty());
    }

    #[test]
    fn un_arrivant_rejoint_la_video_en_cours() {
        let s = screen_at(0);
        let id = s.id();
        let mut ps = vec![
            Player::new(A, Vec3::new(0.0, 65.0, 6.0)),
            Player::new(B, Vec3::new(500.0, 65.0, 6.0)),
        ];
        ps[0].local.push(s);
        run(&mut ps, 0.5);
        ps[0].page(id).act(&Act::Open(id, watch_url(X, 0.0)));
        run(&mut ps, 30.0);
        // Loin : Bob n'en sait rien.
        assert!(ps[1].party.screens(&[]).is_empty());
        ps[1].pos = Vec3::new(2.0, 65.0, 10.0);
        run(&mut ps, 8.0);
        let (a, b) = (ps[0].page(id).t, ps[1].page(id).t);
        assert_eq!(ps[1].page(id).video().as_deref(), Some(X));
        assert!(a > 30.0 && close(a, b, 1.0), "Alice {a}, Bob {b}");
        // Bob repart : l'écran disparaît chez lui (plus annoncé).
        ps[1].pos = Vec3::new(500.0, 65.0, 6.0);
        run(&mut ps, 8.0);
        assert!(ps[1].party.screens(&[]).is_empty());
    }

    #[test]
    fn une_pause_juste_apres_l_ouverture_compte() {
        let s = screen_at(0);
        let id = s.id();
        let mut ps = vec![
            Player::new(A, Vec3::new(0.0, 65.0, 6.0)),
            Player::new(B, Vec3::new(3.0, 65.0, 8.0)),
        ];
        ps[0].local.push(s);
        run(&mut ps, 1.0);
        ps[0].page(id).act(&Act::Open(id, watch_url(X, 0.0)));
        run(&mut ps, 3.5);
        assert_eq!(ps[1].page(id).video().as_deref(), Some(X));
        ps[1].page(id).paused = true;
        run(&mut ps, 2.0);
        assert!(ps[0].page(id).paused && ps[1].page(id).paused);
    }

    #[test]
    fn deux_gestes_en_meme_temps_finissent_pareil() {
        let s = screen_at(0);
        let id = s.id();
        let mut ps = vec![
            Player::new(A, Vec3::new(0.0, 65.0, 6.0)),
            Player::new(B, Vec3::new(3.0, 65.0, 8.0)),
        ];
        ps[0].local.push(s);
        run(&mut ps, 1.0);
        ps[0].page(id).act(&Act::Open(id, watch_url(X, 0.0)));
        run(&mut ps, 6.0);
        // Chacun ouvre une autre vidéo au même moment.
        ps[0].page(id).act(&Act::Open(id, watch_url(Y, 0.0)));
        ps[1].page(id).act(&Act::Open(id, watch_url("aaaaaaaaaaa", 0.0)));
        run(&mut ps, 12.0);
        let (va, vb) = (ps[0].page(id).video(), ps[1].page(id).video());
        assert_eq!(va, vb);
        let (a, b) = (ps[0].page(id).t, ps[1].page(id).t);
        assert!(close(a, b, 1.5), "Alice {a}, Bob {b}");
    }

    #[test]
    fn messages_refuses() {
        let mut p = Party::new();
        let s = screen_at(0);
        let msg = |m: &Msg| {
            let mut d = MAGIC.to_vec();
            d.extend(minelib_net::game::encode(m));
            d
        };
        let shared = |s: &Placed, video: Option<Video>| Shared {
            block: s.block,
            normal: s.normal,
            facing: s.facing,
            size: s.size,
            video,
        };
        // Pas pour nous (voix) : rendu à la voix.
        assert!(!p.receive(A, &[0, 1, 2], Some(Vec3::ZERO), &[]));
        // Écran loin de celui qui l'annonce, ou sans savoir où il est : refusé.
        let near_s = msg(&Msg::Screens(vec![shared(&s, None)]));
        assert!(p.receive(A, &near_s, Some(Vec3::new(500.0, 65.0, 0.0)), &[]));
        assert!(p.receive(A, &near_s, None, &[]));
        assert!(p.screens(&[]).is_empty());
        // Écran tordu : refusé.
        let bent = Placed {
            block: [0, 64, 0],
            normal: [1, 1, 0],
            facing: [1, 0, 0],
            size: 1,
        };
        let m = msg(&Msg::Screens(vec![shared(&bent, None)]));
        p.receive(A, &m, Some(Vec3::new(0.0, 65.0, 5.0)), &[]);
        assert!(p.screens(&[]).is_empty());
        // À la place d'un des miens : refusé.
        p.receive(A, &near_s, Some(Vec3::new(0.0, 65.0, 5.0)), &[s]);
        assert!(p.screens(&[s]).is_empty());
        // Vidéo qui n'est pas un numéro YouTube (une adresse) : l'écran oui, la vidéo non.
        let bad = Video {
            id: "https://evil.example/".into(),
            t: 0.0,
            paused: false,
            seq: 1,
            by: A.into(),
            short: false,
            queue: Vec::new(),
            lus: Vec::new(),
        };
        let m = msg(&Msg::Screens(vec![shared(&s, Some(bad))]));
        p.receive(A, &m, Some(Vec3::new(0.0, 65.0, 5.0)), &[]);
        assert_eq!(p.screens(&[]).len(), 1);
        let acts = p.update(0.05, &[], &[]);
        assert!(acts.is_empty(), "{acts:?}");
        // Rang énorme : refusé une fois qu'on en a un.
        let good = |seq| Video {
            id: X.into(),
            t: 1.0,
            paused: false,
            seq,
            by: A.into(),
            short: false,
            queue: Vec::new(),
            lus: Vec::new(),
        };
        let m = msg(&Msg::Video {
            screen: s.id(),
            video: good(1),
        });
        p.receive(A, &m, None, &[]);
        let acts = p.update(0.05, &[], &[]);
        assert!(matches!(&acts[..], [Act::Open(_, u)] if u.contains(X)), "{acts:?}");
        let m = msg(&Msg::Video {
            screen: s.id(),
            video: Video {
                id: Y.into(),
                ..good(SEQ_MAX - 2)
            },
        });
        p.receive(B, &m, None, &[]);
        assert!(p.update(0.05, &[], &[]).is_empty());
        // Écran inconnu : rien.
        let m = msg(&Msg::Video {
            screen: 12345,
            video: good(5),
        });
        p.receive(B, &m, None, &[]);
        assert!(!p.tracks.contains_key(&12345));
    }

    #[test]
    fn la_taille_de_l_ecran_arrive_chez_les_autres() {
        let giant = Placed::new([0, 64, 0], [0, 0, 1], Vec3::new(0.0, 65.0, 6.0), 3);
        let mut ps = vec![
            Player::new(A, Vec3::new(0.0, 65.0, 6.0)),
            Player::new(B, Vec3::new(3.0, 65.0, 8.0)),
        ];
        ps[0].local.push(giant);
        run(&mut ps, 1.0);
        assert_eq!(ps[1].party.screens(&[]), vec![(giant, A.to_string())]);
        // Taille inconnue : refusée.
        let mut p = Party::new();
        let mut d = MAGIC.to_vec();
        let odd = Shared {
            block: [0, 64, 0],
            normal: [0, 0, 1],
            facing: [0, 0, 1],
            size: 9,
            video: None,
        };
        d.extend(minelib_net::game::encode(&Msg::Screens(vec![odd])));
        p.receive(A, &d, Some(Vec3::new(0.0, 65.0, 5.0)), &[]);
        assert!(p.screens(&[]).is_empty());
        // Ancien format (sans taille) : pas pour nous, rendu à la voix.
        assert!(!p.receive(A, &[0xFF, b'M', b'T', b'V', 0], Some(Vec3::ZERO), &[]));
    }

    /// (v213) Seul devant son écran, une page qui a bloqué un instant n'est jamais avancée
    /// d'un saut : la vidéo partagée la suit. Un ami qui arrive ensuite reçoit le temps où
    /// en est vraiment la page.
    #[test]
    fn seul_devant_l_ecran_la_video_n_est_jamais_sautee() {
        let mut party = Party::new();
        let s = screen_at(0);
        let id = s.id();
        let state = |t: f32, ad: bool| {
            format!(r#"{{"u":"{}","t":{t},"p":false,"a":{ad},"i":"{X}"}}"#, watch_url(X, 0.0))
        };
        party.observe(id, &state(10.0, false), A);
        party.observe(id, &state(10.5, false), A);
        let mut acts = Vec::new();
        // La page bloque 6 s (temps figé, « pub ») puis repart d'où elle était.
        for k in 0..60 {
            party.observe(id, &state(11.0, k > 2), A);
            acts.extend(party.update(0.1, &[s], &[]));
        }
        for k in 0..20 {
            party.observe(id, &state(11.0 + k as f32 * 0.1, false), A);
            acts.extend(party.update(0.1, &[s], &[]));
        }
        assert!(acts.is_empty(), "{acts:?}");
        // Un ami arrive : il reçoit le temps de la page (13 s), pas 19 s.
        let t = party.tracks.get(&id).unwrap();
        assert!((t.now() - 12.9).abs() < 0.3, "{}", t.now());
    }

    #[test]
    fn seul_le_numero_de_la_video_part() {
        // Une page sur un autre site ou une recherche ne part pas chez les autres.
        let s = screen_at(0);
        let id = s.id();
        let mut ps = vec![
            Player::new(A, Vec3::new(0.0, 65.0, 6.0)),
            Player::new(B, Vec3::new(3.0, 65.0, 8.0)),
        ];
        ps[0].local.push(s);
        run(&mut ps, 1.0);
        ps[0].page(id).url = "https://evil.example/watch?v=dQw4w9WgXcQ".into();
        ps[0].page(id).t = 5.0;
        ps[0].page(id).paused = false;
        run(&mut ps, 3.0);
        assert_eq!(ps[1].page(id).url, "https://www.youtube.com/");
        assert!(ps[1].party.tracks.is_empty());
    }
}
