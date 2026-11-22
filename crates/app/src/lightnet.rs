//! (v224) Lumières de l'éditeur sur un serveur de construction : elles font partie de la
//! carte (le paquet que reçoit chaque joueur les contient, voir `fpsmap::package`), et les
//! OP du serveur les posent, les changent et les retirent pour tout le monde.
//!
//! Comme les objets au sol (voir `minelib_net::items`), tout passe par l'arbitre du chunk de
//! chaque lumière : le relais du monde, ou le relais de sa province. Un OP demande
//! (`BuildMsg::LightPut`) ; l'arbitre vérifie qu'il est OP et publie (`BuildMsg::Light`) : la
//! dernière de chaque lumière est gardée dans l'historique de sa zone, remise aux joueurs
//! proches (ceux qui arrivent la reçoivent en s'approchant, avec le reste de la zone) et au
//! propriétaire, qui l'enregistre avec le monde. Un refus (`BuildMsg::LightDenied`) ne va
//! qu'au demandeur.
//!
//! Une lumière vit dans la zone où elle se trouve. Retirée (`light: None`), elle ne l'est que
//! de cette zone : une lumière qui passe d'une zone à l'autre est d'abord mise dans la
//! nouvelle, puis, quand l'arbitre l'a acceptée, retirée de l'ancienne (`cleanup`). Un refus
//! n'en perd donc jamais aucune.
//!
//! Côté joueur, l'éditeur change `Lights::scene` comme en solo. Ce qui y diffère de ce que
//! les autres en savent (`told`) part en demande, au plus `EVERY` pour une même lumière (une
//! lumière qu'on fait glisser part 20 fois par seconde) et `RATE` par seconde en tout. Ce
//! qu'on demande s'affiche tout de suite ; un refus remet la lumière comme l'arbitre l'a
//! publiée (`confirmed`). Ce que publient les autres OP arrive dans la scène, sauf pour une
//! lumière dont on attend encore la réponse : l'arbitre publie dans l'ordre où il reçoit, et
//! notre demande, partie après, passera après la leur.

use std::collections::{BTreeMap, HashMap, HashSet};

use minelib_net::game::BuildMsg;
use minelib_net::proto;

use crate::lights::{PlacedLight, MAX_PLACED};

/// Demandes au plus par seconde (et d'un coup), toutes lumières comprises.
pub const RATE: f32 = 100.0;
/// Temps au plus entre deux demandes pour une même lumière (s).
pub const EVERY: f64 = 0.05;
/// Une lumière à retirer d'une zone qu'elle a quittée, refusée : redemandée après ce temps
/// (s), au plus `CLEANUP_TRIES` fois.
const CLEANUP_RETRY: f64 = 1.0;
const CLEANUP_TRIES: u8 = 5;
/// Une demande sans réponse depuis ce temps (s) est oubliée (relais changé en route) : on
/// n'attend plus pour cette lumière ce que publient les autres.
const ASK_TIMEOUT: f64 = 10.0;

/// Zone (historique du relais) d'un chunk.
fn zone(chunk: [i32; 2]) -> [i32; 2] {
    proto::zone_of(chunk)
}

/// Ce qu'une demande sans réponse doit faire.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Ask {
    /// Mettre la lumière dans sa zone (`from` : la zone qu'elle quitte, à vider ensuite).
    Put { uid: u64, from: Option<[i32; 2]> },
    /// La retirer (supprimée par le joueur).
    Remove { uid: u64 },
    /// La retirer d'une zone qu'elle a quittée.
    Cleanup {
        uid: u64,
        chunk: [i32; 2],
        tries: u8,
    },
}

impl Ask {
    fn uid(&self) -> u64 {
        match *self {
            Ask::Put { uid, .. } | Ask::Remove { uid } | Ask::Cleanup { uid, .. } => uid,
        }
    }
}

/// Ce qu'un message de l'arbitre a changé dans la scène.
#[derive(Default, Debug, PartialEq)]
pub struct Heard {
    /// La scène a changé (à enregistrer, et les lumières chargées à relever).
    pub changed: bool,
    /// Indices retirés de la scène, dans l'ordre (la sélection de l'éditeur les suit).
    pub removed: Vec<usize>,
    /// Une de nos demandes a été refusée.
    pub denied: bool,
}

impl Heard {
    fn add(&mut self, o: Heard) {
        self.changed |= o.changed;
        self.removed.extend(o.removed);
        self.denied |= o.denied;
    }
}

/// Partage des lumières d'un joueur sur un serveur de construction.
pub struct LightSync {
    me: String,
    /// Ce que les autres savent de chaque lumière d'après nous (la carte, ce que l'arbitre a
    /// publié, ce qu'on lui a demandé) : des copies de la scène, avec le même `id`.
    told: HashMap<u64, PlacedLight>,
    /// Ce que l'arbitre a publié de chaque lumière (ou la carte) : remis sur un refus.
    confirmed: HashMap<u64, PlacedLight>,
    /// Demandes sans réponse, et quand elles sont parties (horloge `clock`).
    asked: BTreeMap<u32, (Ask, f64)>,
    /// Zones à vider de lumières qui les ont quittées : (lumière, chunk, essais, quand).
    cleanup: Vec<(u64, [i32; 2], u8, f64)>,
    /// Dernière demande de chaque lumière (horloge `clock`).
    sent_at: HashMap<u64, f64>,
    next_seq: u32,
    budget: f32,
    clock: f64,
    /// Dernier changement de la scène vu (`Lights::scene_gen`), et des changements restent à
    /// envoyer (une lumière envoyée il y a moins de `EVERY`, ou plus de budget).
    gen_seen: Option<u64>,
    behind: bool,
}

impl LightSync {
    /// Partage des lumières de `scene` (celles de la carte : les autres ont les mêmes) pour
    /// le joueur `me`.
    pub fn new(me: &str, scene: &[PlacedLight]) -> Self {
        let told: HashMap<u64, PlacedLight> = scene
            .iter()
            .filter(|l| l.uid != 0)
            .map(|l| (l.uid, l.clone()))
            .collect();
        Self {
            me: me.to_string(),
            confirmed: told.clone(),
            told,
            asked: BTreeMap::new(),
            cleanup: Vec::new(),
            sent_at: HashMap::new(),
            // (Tiré au hasard : l'arbitre ne confond pas avec une session d'avant.)
            next_seq: rand::random::<u32>(),
            budget: RATE,
            clock: 0.0,
            gen_seen: None,
            behind: true,
        }
    }

    /// Demandes sans réponse.
    #[cfg(test)]
    pub fn pending(&self) -> usize {
        self.asked.len()
    }

    fn waiting(&self, uid: u64) -> bool {
        self.asked.values().any(|a| a.0.uid() == uid)
    }

    fn put(&mut self, ask: Ask, chunk: [i32; 2], light: Option<&PlacedLight>) -> BuildMsg {
        self.next_seq = self.next_seq.wrapping_add(1).max(1);
        let seq = self.next_seq;
        self.asked.insert(seq, (ask, self.clock));
        BuildMsg::LightPut {
            seq,
            uid: ask.uid(),
            chunk,
            light: light.map(|l| l.to_text().into_bytes()),
        }
    }

    /// Une image (`dt` s) : ce qui a changé dans la scène depuis ce que les autres en savent,
    /// en demandes à envoyer (`BuildMsg::LightPut`, chacune à l'arbitre de son chunk).
    /// `gen` : `Lights::scene_gen` (rien n'est comparé s'il n'a pas changé).
    pub fn outgoing(&mut self, scene: &[PlacedLight], gen: u64, dt: f32) -> Vec<BuildMsg> {
        self.clock += dt as f64;
        self.budget = (self.budget + RATE * dt).min(RATE);
        let now = self.clock;
        self.asked.retain(|_, a| now - a.1 < ASK_TIMEOUT);
        let mut out = Vec::new();
        // Zones quittées à vider (quand l'arbitre a accepté la lumière ailleurs).
        let due: Vec<(u64, [i32; 2], u8)> = {
            let mut due = Vec::new();
            self.cleanup.retain(|&(uid, chunk, tries, at)| {
                if at <= now {
                    due.push((uid, chunk, tries));
                    false
                } else {
                    true
                }
            });
            due
        };
        for (uid, chunk, tries) in due {
            // Revenue entre-temps dans cette zone : rien à vider.
            if self
                .told
                .get(&uid)
                .is_some_and(|t| zone(t.chunk()) == zone(chunk))
            {
                continue;
            }
            if self.budget < 1.0 {
                self.cleanup.push((uid, chunk, tries, now));
                continue;
            }
            self.budget -= 1.0;
            out.push(self.put(Ask::Cleanup { uid, chunk, tries }, chunk, None));
        }
        if self.gen_seen == Some(gen) && !self.behind {
            return out;
        }
        self.gen_seen = Some(gen);
        self.behind = false;
        let mut here = HashSet::new();
        for l in scene.iter().filter(|l| l.uid != 0) {
            here.insert(l.uid);
            let old = self.told.get(&l.uid);
            if old == Some(l) {
                continue;
            }
            if self
                .sent_at
                .get(&l.uid)
                .is_some_and(|&t| self.clock - t < EVERY)
                || self.budget < 1.0
            {
                self.behind = true;
                continue;
            }
            let from = old
                .map(|o| o.chunk())
                .filter(|&c| zone(c) != zone(l.chunk()));
            self.budget -= 1.0;
            let m = self.put(Ask::Put { uid: l.uid, from }, l.chunk(), Some(l));
            out.push(m);
            self.told.insert(l.uid, l.clone());
            self.sent_at.insert(l.uid, self.clock);
        }
        // Retirées par le joueur.
        let gone: Vec<u64> = self
            .told
            .keys()
            .filter(|u| !here.contains(*u))
            .copied()
            .collect();
        for uid in gone {
            if self.budget < 1.0 {
                self.behind = true;
                break;
            }
            let Some(o) = self.told.remove(&uid) else {
                continue;
            };
            self.budget -= 1.0;
            self.sent_at.insert(uid, self.clock);
            out.push(self.put(Ask::Remove { uid }, o.chunk(), None));
        }
        out
    }

    /// Message de l'arbitre (`Light`, `LightDenied` ; le reste est ignoré) : la scène suit.
    /// `new_id` : identifiant de partie d'une lumière qui arrive.
    pub fn hear(
        &mut self,
        m: &BuildMsg,
        scene: &mut Vec<PlacedLight>,
        new_id: &mut dyn FnMut() -> u64,
    ) -> Heard {
        match m {
            BuildMsg::Light {
                uid,
                chunk,
                light,
                by,
                seq,
            } => {
                let mine = by.eq_ignore_ascii_case(&self.me)
                    && self
                        .asked
                        .get(seq)
                        .is_some_and(|a| a.0.uid() == *uid);
                let ask = if mine {
                    self.asked.remove(seq).map(|a| a.0)
                } else {
                    None
                };
                self.published(*uid, *chunk, light.as_deref(), ask, scene, new_id)
            }
            BuildMsg::LightDenied { seq } => {
                let Some((ask, _)) = self.asked.remove(seq) else {
                    return Heard::default();
                };
                let mut h = Heard {
                    denied: true,
                    ..Default::default()
                };
                match ask {
                    // Une zone quittée à vider : plus tard (la lumière est déjà ailleurs).
                    Ask::Cleanup { uid, chunk, tries } => {
                        if tries + 1 < CLEANUP_TRIES {
                            self.cleanup
                                .push((uid, chunk, tries + 1, self.clock + CLEANUP_RETRY));
                        }
                    }
                    Ask::Put { uid, .. } | Ask::Remove { uid } => {
                        // Une demande plus récente de la même lumière décide.
                        if !self.waiting(uid) {
                            h.add(self.revert(uid, scene, new_id));
                        }
                    }
                }
                h
            }
            _ => Heard::default(),
        }
    }

    /// Ce que l'arbitre vient de publier de la lumière `uid` dans la zone de `chunk` (`text`
    /// `None` : plus dans cette zone) ; `ask` : notre demande, s'il répond à l'une d'elles.
    fn published(
        &mut self,
        uid: u64,
        chunk: [i32; 2],
        text: Option<&[u8]>,
        ask: Option<Ask>,
        scene: &mut Vec<PlacedLight>,
        new_id: &mut dyn FnMut() -> u64,
    ) -> Heard {
        let mut h = Heard::default();
        let z = zone(chunk);
        let light = match text {
            Some(t) => match PlacedLight::from_text(t) {
                Some(mut l) => {
                    l.uid = uid;
                    Some(l)
                }
                // Illisible (version plus récente ?) : rien n'en est gardé.
                None => return h,
            },
            None => None,
        };
        match &light {
            Some(l) => {
                self.confirmed.insert(uid, l.clone());
            }
            None => {
                if self.confirmed.get(&uid).is_some_and(|c| zone(c.chunk()) == z) {
                    self.confirmed.remove(&uid);
                }
            }
        }
        // Notre demande acceptée : la scène l'a déjà ; une lumière mise dans une nouvelle
        // zone est retirée de l'ancienne.
        if let Some(ask) = ask {
            if let Ask::Put {
                uid,
                from: Some(old),
            } = ask
            {
                self.cleanup.push((uid, old, 0, self.clock));
            }
            return h;
        }
        // Une de nos demandes pour cette lumière est encore en route : elle passera après.
        if self.waiting(uid) {
            return h;
        }
        match light {
            Some(mut l) => {
                match scene.iter().position(|x| x.uid == uid) {
                    Some(i) => {
                        l.id = scene[i].id;
                        if scene[i] != l {
                            scene[i] = l.clone();
                            h.changed = true;
                        }
                    }
                    None => {
                        if scene.len() >= MAX_PLACED {
                            return h;
                        }
                        l.id = new_id();
                        scene.push(l.clone());
                        h.changed = true;
                    }
                }
                self.told.insert(uid, l);
            }
            None => {
                if let Some(i) = scene
                    .iter()
                    .position(|x| x.uid == uid && zone(x.chunk()) == z)
                {
                    scene.remove(i);
                    h.changed = true;
                    h.removed.push(i);
                }
                if self.told.get(&uid).is_some_and(|t| zone(t.chunk()) == z) {
                    self.told.remove(&uid);
                }
            }
        }
        h
    }

    /// Refus : la lumière `uid` revient comme l'arbitre l'a publiée (ou disparaît, si les
    /// autres ne l'ont jamais eue).
    fn revert(
        &mut self,
        uid: u64,
        scene: &mut Vec<PlacedLight>,
        new_id: &mut dyn FnMut() -> u64,
    ) -> Heard {
        let mut h = Heard::default();
        let at = scene.iter().position(|x| x.uid == uid);
        match (self.confirmed.get(&uid).cloned(), at) {
            (Some(mut c), Some(i)) => {
                c.id = scene[i].id;
                if scene[i] != c {
                    scene[i] = c.clone();
                    h.changed = true;
                }
                self.told.insert(uid, c);
            }
            (Some(mut c), None) => {
                if scene.len() < MAX_PLACED {
                    c.id = new_id();
                    scene.push(c.clone());
                    h.changed = true;
                    self.told.insert(uid, c);
                }
            }
            (None, Some(i)) => {
                scene.remove(i);
                h.changed = true;
                h.removed.push(i);
                self.told.remove(&uid);
            }
            (None, None) => {
                self.told.remove(&uid);
            }
        }
        h
    }
}

/// La sélection de l'éditeur (indice dans la scène) après le retrait des indices `removed`
/// (dans l'ordre où ils ont été retirés).
pub fn follow_selection(selected: Option<usize>, removed: &[usize]) -> Option<usize> {
    let mut sel = selected;
    for &r in removed {
        sel = match sel {
            Some(s) if s == r => None,
            Some(s) if s > r => Some(s - 1),
            s => s,
        };
    }
    sel
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lights::Kind;

    fn light(uid: u64, x: f64, name: &str) -> PlacedLight {
        PlacedLight {
            id: uid + 1000,
            uid,
            name: name.into(),
            kind: Kind::Spot,
            position: [x, 70.0, 8.0],
            ..Default::default()
        }
    }

    /// Un arbitre d'essai : il accepte tout de `ok`, refuse le reste, et publie dans l'ordre.
    fn arbitrate(reqs: &[BuildMsg], by: &str, ok: bool) -> Vec<BuildMsg> {
        reqs.iter()
            .map(|r| match r {
                BuildMsg::LightPut {
                    seq,
                    uid,
                    chunk,
                    light,
                } if ok => BuildMsg::Light {
                    uid: *uid,
                    chunk: *chunk,
                    light: light.clone(),
                    by: by.into(),
                    seq: *seq,
                },
                BuildMsg::LightPut { seq, .. } => BuildMsg::LightDenied { seq: *seq },
                _ => unreachable!(),
            })
            .collect()
    }

    fn ids() -> impl FnMut() -> u64 {
        let mut n = 5000;
        move || {
            n += 1;
            n
        }
    }

    fn hear_all(
        s: &mut LightSync,
        msgs: &[BuildMsg],
        scene: &mut Vec<PlacedLight>,
    ) -> Heard {
        let mut id = ids();
        let mut h = Heard::default();
        for m in msgs {
            h.add(s.hear(m, scene, &mut id));
        }
        h
    }

    fn names(scene: &[PlacedLight]) -> Vec<String> {
        let mut v: Vec<String> = scene.iter().map(|l| l.name.clone()).collect();
        v.sort();
        v
    }

    /// Un OP pose, change, déplace d'une zone à l'autre et retire des lumières : un autre
    /// joueur, qui a la même carte, les voit comme lui ; rien ne repart en boucle.
    #[test]
    fn an_op_changes_lights_and_another_player_sees_the_same() {
        let base = vec![light(11, 8.0, "A"), light(12, 40.0, "B")];
        let mut op_scene = base.clone();
        let mut other_scene = base.clone();
        let mut op = LightSync::new("Proprio", &op_scene);
        let mut other = LightSync::new("Joueur", &other_scene);
        // Rien n'a changé : rien ne part.
        assert!(op.outgoing(&op_scene, 0, 0.016).is_empty());
        // Il change A, en ajoute une, retire B.
        op_scene[0].intensity = 7.0;
        op_scene.push(light(13, 20.0, "C"));
        op_scene.remove(1);
        let reqs = op.outgoing(&op_scene, 1, 0.016);
        assert_eq!(reqs.len(), 3, "{reqs:?}");
        let pubs = arbitrate(&reqs, "Proprio", true);
        // Sa propre demande acceptée ne change pas sa scène.
        let h = hear_all(&mut op, &pubs, &mut op_scene);
        assert!(!h.changed && op.pending() == 0);
        let h = hear_all(&mut other, &pubs, &mut other_scene);
        assert!(h.changed);
        assert_eq!(names(&other_scene), ["A", "C"]);
        assert_eq!(other_scene.iter().find(|l| l.uid == 11).unwrap().intensity, 7.0);
        // Ce qu'il a reçu ne repart pas.
        assert!(other.outgoing(&other_scene, 1, 0.016).is_empty());
        // Il déplace A dans une autre zone (64 blocs plus loin) : posée dans la nouvelle,
        // puis, une fois acceptée, retirée de l'ancienne.
        op_scene[0].position[0] = 8.0 + 128.0;
        let reqs = op.outgoing(&op_scene, 2, 0.1);
        assert_eq!(reqs.len(), 1);
        let pubs = arbitrate(&reqs, "Proprio", true);
        hear_all(&mut op, &pubs, &mut op_scene);
        let clean = op.outgoing(&op_scene, 2, 0.016);
        assert!(
            matches!(&clean[..], [BuildMsg::LightPut { light: None, chunk, .. }] if chunk[0] == 0),
            "{clean:?}"
        );
        let mut all = pubs.clone();
        all.extend(arbitrate(&clean, "Proprio", true));
        // L'autre la reçoit dans n'importe quel ordre : posée ailleurs, retirée de l'ancienne
        // zone (où elle n'est plus) : une seule, à sa nouvelle place.
        for order in [all.clone(), all.iter().rev().cloned().collect::<Vec<_>>()] {
            let mut sc = other_scene.clone();
            let mut o = LightSync::new("Joueur", &sc);
            hear_all(&mut o, &order, &mut sc);
            let a: Vec<&PlacedLight> = sc.iter().filter(|l| l.uid == 11).collect();
            assert_eq!(a.len(), 1);
            assert_eq!(a[0].position[0], 136.0);
        }
        hear_all(&mut op, &arbitrate(&clean, "Proprio", true), &mut op_scene);
        assert_eq!(op.pending(), 0);
        assert!(op.outgoing(&op_scene, 2, 1.0).is_empty());
    }

    /// Une lumière qu'on fait glisser part au plus 20 fois par seconde, et sa dernière place
    /// part toujours.
    #[test]
    fn a_dragged_light_is_sent_twenty_times_a_second_and_ends_where_it_stops() {
        let mut scene = vec![light(21, 8.0, "A")];
        let mut s = LightSync::new("Proprio", &scene);
        let mut sent = Vec::new();
        let mut gen = 0;
        for k in 0..100 {
            scene[0].position[2] = 8.0 + k as f64 * 0.01;
            gen += 1;
            sent.extend(s.outgoing(&scene, gen, 0.005));
        }
        // Une demi-seconde à 200 images par seconde : 10 envois.
        assert!((9..=11).contains(&sent.len()), "{}", sent.len());
        // On s'arrête : la dernière place part quand même.
        for _ in 0..20 {
            sent.extend(s.outgoing(&scene, gen, 0.005));
        }
        let last = sent.iter().rev().find_map(|m| match m {
            BuildMsg::LightPut { light: Some(t), .. } => PlacedLight::from_text(t),
            _ => None,
        });
        assert_eq!(last.unwrap().position[2], scene[0].position[2]);
    }

    /// Refusé (pas OP) : la lumière revient comme les autres l'ont ; une lumière neuve
    /// disparaît, une lumière retirée revient.
    #[test]
    fn a_refused_change_comes_back_as_the_others_have_it() {
        let base = vec![light(31, 8.0, "A"), light(32, 30.0, "B")];
        let mut scene = base.clone();
        let mut s = LightSync::new("Joueur", &scene);
        scene[0].color = [1.0, 0.0, 0.0];
        scene.push(light(33, 20.0, "C"));
        scene.remove(1);
        let reqs = s.outgoing(&scene, 1, 0.016);
        let h = hear_all(&mut s, &arbitrate(&reqs, "Joueur", false), &mut scene);
        assert!(h.denied && h.changed);
        assert_eq!(names(&scene), ["A", "B"]);
        assert_eq!(scene.iter().find(|l| l.uid == 31).unwrap().color, [1.0; 3]);
        assert!(s.outgoing(&scene, 2, 0.1).is_empty(), "rien ne repart");
    }

    /// Un autre OP change une lumière dont on attend encore la réponse : notre demande,
    /// partie après la sienne, passe après chez l'arbitre ; on garde la nôtre.
    #[test]
    fn our_pending_change_wins_over_an_earlier_one_from_another_op() {
        let mut scene = vec![light(41, 8.0, "A")];
        let mut s = LightSync::new("Moi", &scene);
        scene[0].intensity = 3.0;
        let mine = s.outgoing(&scene, 1, 0.016);
        let mut theirs = light(41, 8.0, "A");
        theirs.intensity = 9.0;
        let other = BuildMsg::Light {
            uid: 41,
            chunk: theirs.chunk(),
            light: Some(theirs.to_text().into_bytes()),
            by: "Autre".into(),
            seq: 7,
        };
        let h = hear_all(&mut s, std::slice::from_ref(&other), &mut scene);
        assert!(!h.changed);
        hear_all(&mut s, &arbitrate(&mine, "Moi", true), &mut scene);
        assert_eq!(scene[0].intensity, 3.0);
        // Plus rien en attente : la suivante, d'un autre, arrive.
        let h = hear_all(&mut s, &[other], &mut scene);
        assert!(h.changed);
        assert_eq!(scene[0].intensity, 9.0);
    }

    #[test]
    fn the_editor_selection_follows_removed_lights() {
        assert_eq!(follow_selection(Some(3), &[1]), Some(2));
        assert_eq!(follow_selection(Some(3), &[3]), None);
        assert_eq!(follow_selection(Some(3), &[5, 0]), Some(2));
        assert_eq!(follow_selection(None, &[0]), None);
    }
}
