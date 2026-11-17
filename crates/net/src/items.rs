//! Objets au sol et coffres des mondes de construction : la même règle partout (jeu seul,
//! relais du monde, relais de région), pour qu'aucun objet ne puisse exister deux fois.
//!
//! Tout passe par des **demandes** (`DropItem`, `TakeItem`, `ChestPut`, `ChestTake`), que
//! seul l'arbitre (le relais ; en solo, le jeu) applique, dans l'ordre où il les reçoit, à
//! l'état qui fait foi (`ItemStore`). Chaque demande acceptée donne des **états publiés**
//! (`Item`, `Slot`) ; une demande refusée donne `Denied`, remis au seul demandeur.
//!
//! Côté joueur (voir `Ledger`), l'inventaire suit une règle simple : **on rend avant de
//! recevoir**. Ce qu'on pose ou lâche quitte l'inventaire tout de suite et n'y revient que
//! sur un refus explicite ; ce qu'on prend n'y entre qu'à la publication qui nous nomme.
//! Une réponse perdue (déconnexion) fait donc au pire perdre un objet, jamais le doubler.
//!
//! Les états publiés suffisent à reconstruire l'arbitre (`absorb`) : un relais qui reprend
//! un monde ou une région retrouve exactement les mêmes objets, y compris ceux qui ont
//! déjà été ramassés (leur numéro ne peut pas resservir).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::game::BuildMsg;

/// Emplacements d'un coffre : 3 lignes de 10.
pub const CHEST_SLOTS: usize = 30;
/// Objets par pile.
pub const MAX_PILE: u8 = 64;
/// Objets au sol gardés par monde (au-delà, les plus anciens disparaissent).
pub const MAX_DROPS: usize = 4096;
/// Coffres garnis par monde (au-delà, on ne peut plus en remplir de nouveau).
pub const MAX_CHESTS: usize = 65_536;

/// Une pile d'objets : code de l'objet (bloc, torche…) et nombre (1 à 64).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pile {
    pub item: u32,
    pub count: u8,
}

/// Objet posé au sol.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Dropped {
    pub pos: [f32; 3],
    pub pile: Pile,
}

/// L'état qui fait foi : objets au sol, coffres, et numéros d'objets déjà utilisés.
#[derive(Default, Clone, Debug)]
pub struct ItemStore {
    /// Objets au sol ; `None` : numéro déjà utilisé (objet ramassé), il ne peut resservir.
    pub drops: BTreeMap<u64, Option<Dropped>>,
    pub chests: BTreeMap<[i32; 3], [Option<Pile>; CHEST_SLOTS]>,
    /// Numéros dans l'ordre où ils sont apparus (les plus anciens partent en premier).
    order: std::collections::VecDeque<u64>,
    /// Dernières demandes traitées de chaque joueur : une demande reçue deux fois n'est
    /// appliquée qu'une fois.
    seen: BTreeMap<String, std::collections::VecDeque<u32>>,
    /// Coffres déjà touchés (demande d'un joueur, coffre cassé) : le propriétaire du monde
    /// ne peut plus y remettre le contenu de sa sauvegarde.
    opened: std::collections::BTreeSet<[i32; 3]>,
}

/// Demandes récentes retenues par joueur.
const SEEN_MAX: usize = 512;

/// Numéros d'objets disparus gardés en mémoire (pour refuser une demande rejouée).
const MAX_GONE: usize = 65_536;

fn valid(p: Pile) -> bool {
    (1..=MAX_PILE).contains(&p.count)
}

fn chunk_of(x: f32, z: f32) -> [i32; 2] {
    [
        (x.floor() as i32).div_euclid(16),
        (z.floor() as i32).div_euclid(16),
    ]
}

/// Chunk où ranger un état publié (pour le remettre aux joueurs proches).
pub fn chunk_of_msg(m: &BuildMsg) -> Option<[i32; 2]> {
    match m {
        BuildMsg::DropItem { pos, .. } => Some(chunk_of(pos[0], pos[2])),
        BuildMsg::Item { at, .. } => Some(chunk_of(at[0], at[2])),
        BuildMsg::ChestPut { pos, .. }
        | BuildMsg::ChestTake { pos, .. }
        | BuildMsg::Slot { pos, .. } => Some([pos[0].div_euclid(16), pos[2].div_euclid(16)]),
        _ => None,
    }
}

/// Numéro d'une demande d'objet.
pub fn request_seq(m: &BuildMsg) -> Option<u32> {
    match *m {
        BuildMsg::DropItem { seq, .. }
        | BuildMsg::TakeItem { seq, .. }
        | BuildMsg::ChestPut { seq, .. }
        | BuildMsg::ChestTake { seq, .. } => Some(seq),
        _ => None,
    }
}

/// Est-ce une demande d'objet (à faire arbitrer) ?
pub fn is_request(m: &BuildMsg) -> bool {
    matches!(
        m,
        BuildMsg::DropItem { .. }
            | BuildMsg::TakeItem { .. }
            | BuildMsg::ChestPut { .. }
            | BuildMsg::ChestTake { .. }
    )
}

impl ItemStore {
    /// Note l'état de l'objet `id` ; les plus vieux numéros disparus sont oubliés.
    fn remember(&mut self, id: u64, d: Option<Dropped>) {
        if self.drops.insert(id, d).is_none() {
            self.order.push_back(id);
        }
        while self.order.len() > MAX_GONE + MAX_DROPS {
            let Some(old) = self.order.pop_front() else {
                break;
            };
            if matches!(self.drops.get(&old), Some(Some(_))) {
                // Encore au sol : il garde sa place.
                self.order.push_back(old);
                break;
            }
            self.drops.remove(&old);
        }
    }

    /// Chunk de ce que vise une demande (là où tombe l'objet, l'objet à ramasser, le
    /// coffre) ; `None` : objet inconnu.
    pub fn target_chunk(&self, req: &BuildMsg) -> Option<[i32; 2]> {
        match req {
            BuildMsg::TakeItem { id, .. } => {
                let d = self.drops.get(id).copied().flatten()?;
                Some(chunk_of(d.pos[0], d.pos[2]))
            }
            _ => chunk_of_msg(req),
        }
    }

    /// Objets au sol présents.
    pub fn live_drops(&self) -> impl Iterator<Item = (u64, &Dropped)> {
        self.drops
            .iter()
            .filter_map(|(&id, d)| d.as_ref().map(|d| (id, d)))
    }

    pub fn chest(&self, pos: [i32; 3]) -> [Option<Pile>; CHEST_SLOTS] {
        self.chests
            .get(&pos)
            .copied()
            .unwrap_or([None; CHEST_SLOTS])
    }

    /// Applique la demande de `by`. `Ok` : états à publier ; `Err` : numéro de la demande
    /// refusée (rien n'a changé).
    pub fn apply(&mut self, req: &BuildMsg, by: &str) -> Result<Vec<BuildMsg>, u32> {
        let seq = match *req {
            BuildMsg::DropItem { seq, .. }
            | BuildMsg::TakeItem { seq, .. }
            | BuildMsg::ChestPut { seq, .. }
            | BuildMsg::ChestTake { seq, .. } => seq,
            _ => return Err(0),
        };
        // Demande déjà traitée (reçue deux fois) : ignorée sans réponse, la première
        // réponse fait foi. Un refus ici ferait rendre au joueur un objet déjà posé.
        let seen = self.seen.entry(by.to_string()).or_default();
        if seen.contains(&seq) {
            return Ok(Vec::new());
        }
        seen.push_back(seq);
        if seen.len() > SEEN_MAX {
            seen.pop_front();
        }
        self.apply_new(req, by)
    }

    /// Cette demande de `by` a-t-elle déjà été traitée (reçue deux fois) ?
    pub fn is_duplicate(&self, req: &BuildMsg, by: &str) -> bool {
        request_seq(req).is_some_and(|seq| self.seen.get(by).is_some_and(|s| s.contains(&seq)))
    }

    /// Le joueur `by` repart de zéro (nouvelle connexion) : ses anciens numéros de
    /// demande sont oubliés.
    pub fn forget(&mut self, by: &str) {
        self.seen.remove(by);
    }

    fn apply_new(&mut self, req: &BuildMsg, by: &str) -> Result<Vec<BuildMsg>, u32> {
        match *req {
            BuildMsg::DropItem { seq, id, pos, pile } => {
                let sane = pos.iter().all(|v| v.is_finite() && v.abs() < 4.0e7);
                // Numéro déjà vu (même ramassé depuis) : la demande est un doublon.
                if !valid(pile) || !sane || self.drops.contains_key(&id) {
                    return Err(seq);
                }
                let mut out = Vec::new();
                // Trop d'objets au sol : le plus ancien encore là disparaît.
                if self.live_drops().count() >= MAX_DROPS {
                    let oldest = self
                        .order
                        .iter()
                        .copied()
                        .find(|i| matches!(self.drops.get(i), Some(Some(_))));
                    if let Some(old) = oldest {
                        let at = self.drops[&old].map_or(pos, |d| d.pos);
                        self.drops.insert(old, None);
                        out.push(BuildMsg::Item {
                            id: old,
                            at,
                            drop: None,
                            to: String::new(),
                            seq: 0,
                            got: None,
                        });
                    }
                }
                self.remember(id, Some(Dropped { pos, pile }));
                out.push(BuildMsg::Item {
                    id,
                    at: pos,
                    drop: Some(pile),
                    to: by.to_string(),
                    seq,
                    got: None,
                });
                Ok(out)
            }
            BuildMsg::TakeItem { seq, id } => {
                let Some(Some(d)) = self.drops.get(&id).copied() else {
                    return Err(seq);
                };
                self.drops.insert(id, None);
                Ok(vec![BuildMsg::Item {
                    id,
                    at: d.pos,
                    drop: None,
                    to: by.to_string(),
                    seq,
                    got: Some(d.pile),
                }])
            }
            BuildMsg::ChestPut {
                seq,
                pos,
                slot,
                pile,
            } => {
                let i = slot as usize;
                if !valid(pile) || i >= CHEST_SLOTS {
                    return Err(seq);
                }
                if !self.chests.contains_key(&pos) && self.chests.len() >= MAX_CHESTS {
                    return Err(seq);
                }
                self.opened.insert(pos);
                let mut chest = self.chest(pos);
                let new = match chest[i] {
                    None => pile,
                    Some(cur) if cur.item == pile.item && cur.count + pile.count <= MAX_PILE => {
                        Pile {
                            item: cur.item,
                            count: cur.count + pile.count,
                        }
                    }
                    Some(_) => return Err(seq),
                };
                chest[i] = Some(new);
                self.chests.insert(pos, chest);
                Ok(vec![BuildMsg::Slot {
                    pos,
                    slot,
                    pile: Some(new),
                    by: by.to_string(),
                    seq,
                    got: None,
                }])
            }
            BuildMsg::ChestTake {
                seq,
                pos,
                slot,
                count,
            } => {
                let i = slot as usize;
                self.opened.insert(pos);
                let cur = self.chests.get(&pos).and_then(|c| *c.get(i)?);
                let Some(cur) = cur.filter(|c| count >= 1 && c.count >= count) else {
                    return Err(seq);
                };
                let left = (cur.count > count).then_some(Pile {
                    item: cur.item,
                    count: cur.count - count,
                });
                let chest = self.chests.get_mut(&pos).expect("coffre");
                chest[i] = left;
                if chest.iter().all(Option::is_none) {
                    self.chests.remove(&pos);
                }
                Ok(vec![BuildMsg::Slot {
                    pos,
                    slot,
                    pile: left,
                    by: by.to_string(),
                    seq,
                    got: Some(Pile {
                        item: cur.item,
                        count,
                    }),
                }])
            }
            _ => Err(0),
        }
    }

    /// Le bloc du coffre en `pos` a disparu : son contenu tombe au sol (états à publier).
    /// `next_id` donne un numéro d'objet neuf.
    pub fn spill(&mut self, pos: [i32; 3], mut next_id: impl FnMut() -> u64) -> Vec<BuildMsg> {
        let Some(chest) = self.chests.remove(&pos) else {
            return Vec::new();
        };
        self.opened.insert(pos);
        let mut out = Vec::new();
        let at = [pos[0] as f32 + 0.5, pos[1] as f32, pos[2] as f32 + 0.5];
        for (i, pile) in chest.into_iter().enumerate() {
            let Some(pile) = pile else { continue };
            out.push(BuildMsg::Slot {
                pos,
                slot: i as u8,
                pile: None,
                by: String::new(),
                seq: 0,
                got: None,
            });
            let mut id = next_id();
            while self.drops.contains_key(&id) {
                id = next_id();
            }
            self.remember(id, Some(Dropped { pos: at, pile }));
            out.push(BuildMsg::Item {
                id,
                at,
                drop: Some(pile),
                to: String::new(),
                seq: 0,
                got: None,
            });
        }
        out
    }

    /// Le propriétaire du monde remet un état de sa sauvegarde (à l'ouverture) : accepté
    /// seulement s'il ne peut rien doubler (objet au numéro jamais vu ; emplacement vide
    /// d'un coffre que personne n'a touché depuis). Renvoie l'état à publier.
    pub fn seed(&mut self, saved: &BuildMsg) -> Option<BuildMsg> {
        match *saved {
            BuildMsg::Item {
                id,
                at,
                drop: Some(pile),
                ..
            } => {
                let sane = at.iter().all(|v| v.is_finite() && v.abs() < 4.0e7);
                if !valid(pile) || !sane || self.drops.contains_key(&id) {
                    return None;
                }
                if self.live_drops().count() >= MAX_DROPS {
                    return None;
                }
                self.remember(id, Some(Dropped { pos: at, pile }));
                Some(BuildMsg::Item {
                    id,
                    at,
                    drop: Some(pile),
                    to: String::new(),
                    seq: 0,
                    got: None,
                })
            }
            BuildMsg::Slot {
                pos,
                slot,
                pile: Some(pile),
                ..
            } => {
                let i = slot as usize;
                if !valid(pile) || i >= CHEST_SLOTS || self.opened.contains(&pos) {
                    return None;
                }
                let mut chest = self.chest(pos);
                if chest[i].is_some() {
                    return None;
                }
                chest[i] = Some(pile);
                self.chests.insert(pos, chest);
                Some(BuildMsg::Slot {
                    pos,
                    slot,
                    pile: Some(pile),
                    by: String::new(),
                    seq: 0,
                    got: None,
                })
            }
            _ => None,
        }
    }

    /// Prend en compte un état publié (copie chez un joueur, relais qui reprend un monde).
    pub fn absorb(&mut self, published: &BuildMsg) {
        match published {
            BuildMsg::Item { id, at, drop, .. } => {
                self.remember(*id, drop.map(|pile| Dropped { pos: *at, pile }));
            }
            BuildMsg::Slot {
                pos, slot, pile, ..
            } => {
                let i = *slot as usize;
                if i >= CHEST_SLOTS {
                    return;
                }
                let mut chest = self.chest(*pos);
                chest[i] = *pile;
                if chest.iter().all(Option::is_none) {
                    self.chests.remove(pos);
                } else {
                    self.chests.insert(*pos, chest);
                }
            }
            _ => {}
        }
    }

    /// Tous les états, pour les redonner (sauvegarde, nouveau relais).
    pub fn snapshot(&self) -> Vec<BuildMsg> {
        let mut out = Vec::new();
        for (id, d) in self.live_drops() {
            out.push(BuildMsg::Item {
                id,
                at: d.pos,
                drop: Some(d.pile),
                to: String::new(),
                seq: 0,
                got: None,
            });
        }
        for (pos, chest) in &self.chests {
            for (i, pile) in chest.iter().enumerate() {
                if pile.is_some() {
                    out.push(BuildMsg::Slot {
                        pos: *pos,
                        slot: i as u8,
                        pile: *pile,
                        by: String::new(),
                        seq: 0,
                        got: None,
                    });
                }
            }
        }
        out
    }

    /// Nombre total d'objets de chaque sorte (au sol et dans les coffres).
    pub fn totals(&self) -> BTreeMap<u32, u64> {
        let mut t = BTreeMap::new();
        for (_, d) in self.live_drops() {
            *t.entry(d.pile.item).or_default() += d.pile.count as u64;
        }
        for chest in self.chests.values() {
            for p in chest.iter().flatten() {
                *t.entry(p.item).or_default() += p.count as u64;
            }
        }
        t
    }
}

/// Ce qu'un joueur doit faire de son inventaire après un message reçu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Credit {
    /// Ajouter cette pile à l'inventaire (objet ramassé, pris dans un coffre, ou rendu
    /// après un refus).
    Give(Pile),
}

/// Comptes d'un joueur : ce qu'il a déjà retiré de son inventaire en attendant la réponse
/// de l'arbitre. Règle : on rend avant de recevoir, on ne reprend que sur un refus.
#[derive(Default, Clone, Debug)]
pub struct Ledger {
    next_seq: u32,
    /// Demandes en attente : numéro → pile déjà sortie de l'inventaire (à rendre sur refus).
    owed: BTreeMap<u32, Option<Pile>>,
}

impl Ledger {
    /// Comptes neufs, dont les numéros de demande partent de `start` (tiré au hasard à
    /// chaque session : l'arbitre ne les confond pas avec ceux d'une session d'avant).
    pub fn starting_at(start: u32) -> Self {
        Self {
            next_seq: start,
            owed: BTreeMap::new(),
        }
    }

    fn seq(&mut self) -> u32 {
        self.next_seq = self.next_seq.wrapping_add(1).max(1);
        self.next_seq
    }

    /// Demandes sans réponse.
    pub fn pending(&self) -> usize {
        self.owed.len()
    }

    /// Lâcher `pile` (déjà retirée de l'inventaire par l'appelant) en `pos`.
    pub fn drop_item(&mut self, id: u64, pos: [f32; 3], pile: Pile) -> BuildMsg {
        let seq = self.seq();
        self.owed.insert(seq, Some(pile));
        BuildMsg::DropItem { seq, id, pos, pile }
    }

    /// Ramasser l'objet `id` (l'inventaire ne change qu'à la réponse).
    pub fn take_item(&mut self, id: u64) -> BuildMsg {
        let seq = self.seq();
        self.owed.insert(seq, None);
        BuildMsg::TakeItem { seq, id }
    }

    /// Poser `pile` (déjà retirée de l'inventaire) dans un coffre.
    pub fn chest_put(&mut self, pos: [i32; 3], slot: u8, pile: Pile) -> BuildMsg {
        let seq = self.seq();
        self.owed.insert(seq, Some(pile));
        BuildMsg::ChestPut {
            seq,
            pos,
            slot,
            pile,
        }
    }

    /// Prendre dans un coffre (l'inventaire ne change qu'à la réponse).
    pub fn chest_take(&mut self, pos: [i32; 3], slot: u8, count: u8) -> BuildMsg {
        let seq = self.seq();
        self.owed.insert(seq, None);
        BuildMsg::ChestTake {
            seq,
            pos,
            slot,
            count,
        }
    }

    /// Message reçu de l'arbitre : ce que `me` gagne. Chaque demande n'est créditée
    /// qu'une fois (un message rejoué ne donne rien).
    pub fn on_message(&mut self, m: &BuildMsg, me: &str) -> Option<Credit> {
        match m {
            BuildMsg::Denied { seq, .. } => {
                // Refus : ce qui était sorti de l'inventaire y revient.
                self.owed.remove(seq)?.map(Credit::Give)
            }
            // Ma demande acceptée : objet lâché (rien à gagner) ou ramassé (`got`).
            BuildMsg::Item { to, seq, got, .. } if to.eq_ignore_ascii_case(me) => {
                let known = self.owed.remove(seq).is_some();
                got.filter(|_| known).map(Credit::Give)
            }
            BuildMsg::Slot { by, seq, got, .. } if by.eq_ignore_ascii_case(me) => {
                let known = self.owed.remove(seq).is_some();
                got.filter(|_| known).map(Credit::Give)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STONE: u32 = 1;
    const WOOD: u32 = 5;

    fn pile(item: u32, count: u8) -> Pile {
        Pile { item, count }
    }

    /// Un joueur d'essai : son inventaire (objets par sorte) et ses comptes.
    #[derive(Default)]
    struct Player {
        name: String,
        bag: BTreeMap<u32, u64>,
        ledger: Ledger,
    }

    impl Player {
        fn new(name: &str, stock: &[(u32, u64)]) -> Self {
            Self {
                name: name.into(),
                bag: stock.iter().copied().collect(),
                ledger: Ledger::default(),
            }
        }
        /// Sort `pile` de l'inventaire si elle y est.
        fn spend(&mut self, p: Pile) -> bool {
            let have = self.bag.entry(p.item).or_default();
            if *have < p.count as u64 {
                return false;
            }
            *have -= p.count as u64;
            true
        }
        fn hear(&mut self, m: &BuildMsg) {
            if let Some(Credit::Give(p)) = self.ledger.on_message(m, &self.name) {
                *self.bag.entry(p.item).or_default() += p.count as u64;
            }
        }
    }

    fn total(players: &[Player], store: &ItemStore, item: u32) -> u64 {
        players
            .iter()
            .map(|p| p.bag.get(&item).copied().unwrap_or(0))
            .sum::<u64>()
            + store.totals().get(&item).copied().unwrap_or(0)
    }

    /// Générateur simple et reproductible.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self, n: u64) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (self.0 >> 33) % n
        }
    }

    /// L'arbitre traite `req` de `who` et rend ce qu'il faut remettre (états, ou refus).
    fn arbitrate(store: &mut ItemStore, req: &BuildMsg, who: &str) -> Vec<BuildMsg> {
        match store.apply(req, who) {
            Ok(v) => v,
            Err(seq) => vec![BuildMsg::Denied { seq }],
        }
    }

    #[test]
    fn a_dropped_item_goes_to_exactly_one_of_two_players_who_grab_it() {
        let mut store = ItemStore::default();
        let mut a = Player::new("a", &[(STONE, 10)]);
        let mut b = Player::new("b", &[]);
        let mut c = Player::new("c", &[]);
        assert!(a.spend(pile(STONE, 4)));
        let req = a.ledger.drop_item(77, [0.5, 64.0, 0.5], pile(STONE, 4));
        for m in arbitrate(&mut store, &req, "a") {
            a.hear(&m);
            b.hear(&m);
            c.hear(&m);
        }
        assert_eq!(a.bag[&STONE], 6);
        assert_eq!(store.totals()[&STONE], 4);
        // Les deux demandes partent en même temps : l'arbitre les traite l'une après
        // l'autre, la seconde est refusée.
        let rb = b.ledger.take_item(77);
        let rc = c.ledger.take_item(77);
        let out_b = arbitrate(&mut store, &rb, "b");
        let out_c = arbitrate(&mut store, &rc, "c");
        assert!(matches!(out_c[..], [BuildMsg::Denied { .. }]));
        for m in &out_b {
            // Le même message rejoué trois fois (réseau) : crédité une seule fois.
            for _ in 0..3 {
                a.hear(m);
                b.hear(m);
                c.hear(m);
            }
        }
        c.hear(&out_c[0]);
        assert_eq!(b.bag[&STONE], 4);
        assert_eq!(c.bag.get(&STONE), None);
        assert!(store.totals().is_empty());
        assert_eq!(total(&[a, b, c], &store, STONE), 10);
    }

    #[test]
    fn a_replayed_or_forged_request_never_creates_an_item() {
        let mut store = ItemStore::default();
        let mut a = Player::new("a", &[(WOOD, 3)]);
        assert!(a.spend(pile(WOOD, 3)));
        let req = a.ledger.drop_item(5, [0.0; 3], pile(WOOD, 3));
        assert!(store.apply(&req, "a").is_ok());
        // La même demande reçue deux fois (réseau) : ignorée, sans effet ni refus.
        assert_eq!(store.apply(&req, "a").map(|v| v.len()), Ok(0));
        assert_eq!(store.totals()[&WOOD], 3);
        // Refaite sous un autre numéro de demande, ou par un autre joueur (joueur
        // malin) : refusée, l'objet existe déjà.
        let again = BuildMsg::DropItem {
            seq: 4000,
            id: 5,
            pos: [0.0; 3],
            pile: pile(WOOD, 3),
        };
        assert!(store.apply(&again, "a").is_err());
        assert!(store.apply(&req, "b").is_err());
        // Ramassé, puis redemandé : le numéro d'objet ne peut pas resservir, et l'objet
        // ne se ramasse pas deux fois.
        let take = a.ledger.take_item(5);
        assert_eq!(store.apply(&take, "a").map(|v| v.len()), Ok(1));
        assert_eq!(store.apply(&take, "a").map(|v| v.len()), Ok(0));
        let again = BuildMsg::DropItem {
            seq: 4001,
            id: 5,
            pos: [0.0; 3],
            pile: pile(WOOD, 3),
        };
        assert!(store.apply(&again, "a").is_err());
        assert!(store
            .apply(&BuildMsg::TakeItem { seq: 4002, id: 5 }, "a")
            .is_err());
        assert!(store.totals().is_empty());
        // Piles impossibles, emplacements hors du coffre, positions absurdes.
        let bad = [
            BuildMsg::DropItem {
                seq: 11,
                id: 9,
                pos: [0.0; 3],
                pile: pile(WOOD, 0),
            },
            BuildMsg::DropItem {
                seq: 12,
                id: 9,
                pos: [0.0; 3],
                pile: pile(WOOD, 65),
            },
            BuildMsg::DropItem {
                seq: 13,
                id: 9,
                pos: [f32::NAN, 0.0, 0.0],
                pile: pile(WOOD, 1),
            },
            BuildMsg::ChestPut {
                seq: 14,
                pos: [0; 3],
                slot: 30,
                pile: pile(WOOD, 1),
            },
            BuildMsg::ChestTake {
                seq: 15,
                pos: [0; 3],
                slot: 0,
                count: 1,
            },
            BuildMsg::ChestTake {
                seq: 16,
                pos: [0; 3],
                slot: 0,
                count: 0,
            },
        ];
        for b in &bad {
            assert!(store.apply(b, "a").is_err(), "{b:?}");
        }
        assert!(store.totals().is_empty() && store.chests.is_empty());
    }

    #[test]
    fn chests_hold_thirty_piles_and_two_players_cannot_take_the_same_one() {
        let mut store = ItemStore::default();
        let chest = [3, 70, -2];
        let mut a = Player::new("a", &[(STONE, 64 * 31)]);
        let mut b = Player::new("b", &[]);
        // Trois lignes de dix : trente piles pleines, pas une de plus.
        for slot in 0..31u8 {
            assert!(a.spend(pile(STONE, 64)));
            let req = a.ledger.chest_put(chest, slot, pile(STONE, 64));
            for m in arbitrate(&mut store, &req, "a") {
                a.hear(&m);
            }
        }
        assert_eq!(store.totals()[&STONE], 64 * 30);
        // La 31e, refusée, est revenue dans l'inventaire.
        assert_eq!(a.bag[&STONE], 64);
        assert_eq!(a.ledger.pending(), 0);
        // Pile pleine : on n'y ajoute rien ; autre objet : refusé aussi.
        for p in [pile(STONE, 1), pile(WOOD, 1)] {
            let req = BuildMsg::ChestPut {
                seq: 900 + p.item,
                pos: chest,
                slot: 0,
                pile: p,
            };
            assert!(store.apply(&req, "a").is_err());
        }
        // Les deux prennent tout l'emplacement 7 en même temps : un seul l'a.
        let ra = a.ledger.chest_take(chest, 7, 64);
        let rb = b.ledger.chest_take(chest, 7, 64);
        let oa = arbitrate(&mut store, &ra, "a");
        let ob = arbitrate(&mut store, &rb, "b");
        for m in oa.iter().chain(&ob) {
            a.hear(m);
            b.hear(m);
            a.hear(m);
            b.hear(m);
        }
        assert_eq!(a.bag[&STONE], 128);
        assert_eq!(b.bag.get(&STONE), None);
        // Prendre plus que ce qu'il y a : refusé ; une partie : le reste demeure.
        let r = b.ledger.chest_take(chest, 8, 65);
        assert!(store.apply(&r, "b").is_err());
        let r = b.ledger.chest_take(chest, 8, 10);
        for m in arbitrate(&mut store, &r, "b") {
            b.hear(&m);
        }
        assert_eq!(b.bag[&STONE], 10);
        assert_eq!(store.chest(chest)[8], Some(pile(STONE, 54)));
        assert_eq!(total(&[a, b], &store, STONE), 64 * 31);
    }

    #[test]
    fn breaking_a_chest_spills_its_content_once() {
        let mut store = ItemStore::default();
        let chest = [0, 64, 0];
        for (slot, p) in [(0u8, pile(STONE, 64)), (29, pile(WOOD, 7))] {
            let req = BuildMsg::ChestPut {
                seq: 1 + slot as u32,
                pos: chest,
                slot,
                pile: p,
            };
            store.apply(&req, "a").unwrap();
        }
        let before = store.totals();
        let mut n = 100;
        let out = store.spill(chest, || {
            n += 1;
            n
        });
        // Deux emplacements vidés, deux objets au sol.
        assert_eq!(out.len(), 4);
        assert!(store.chests.is_empty());
        assert_eq!(store.totals(), before);
        assert_eq!(store.live_drops().count(), 2);
        // Cassé une seconde fois (message rejoué) : rien de plus.
        assert!(store.spill(chest, || 1).is_empty());
        assert_eq!(store.totals(), before);
        // Un autre relais qui n'a vu que les états publiés retrouve le même monde.
        let mut copy = ItemStore::default();
        for m in &out {
            copy.absorb(m);
        }
        assert_eq!(copy.totals(), before);
    }

    /// Beaucoup de joueurs font n'importe quoi en même temps sur un réseau qui perd,
    /// retarde, rejoue et mélange les messages : jamais un objet de plus qu'au départ, et
    /// sans perte de message, pas un de moins.
    fn chaos(seed: u64, lossy: bool) {
        let mut rng = Rng(seed);
        let mut store = ItemStore::default();
        let names = ["a", "b", "c", "d", "e"];
        let mut players: Vec<Player> = names
            .iter()
            .map(|n| Player::new(n, &[(STONE, 200), (WOOD, 200)]))
            .collect();
        let start = [
            total(&players, &store, STONE),
            total(&players, &store, WOOD),
        ];
        let chests = [[0, 64, 0], [1, 64, 0]];
        // Messages en route : (destinataire, message).
        let mut wire: Vec<(usize, BuildMsg)> = Vec::new();
        // Demandes en route : (auteur, demande).
        let mut asks: Vec<(usize, BuildMsg)> = Vec::new();
        let mut log: Vec<BuildMsg> = Vec::new();
        let mut next_id = 1u64;
        for step in 0..6000 {
            let who = rng.next(names.len() as u64) as usize;
            let item = if rng.next(2) == 0 { STONE } else { WOOD };
            let count = 1 + rng.next(64) as u8;
            let p = &mut players[who];
            let req = match rng.next(6) {
                0 => p.spend(pile(item, count)).then(|| {
                    next_id += 1;
                    // Parfois un numéro déjà pris (deux joueurs tirent le même).
                    let id = if rng.next(10) == 0 {
                        next_id - 1
                    } else {
                        next_id
                    };
                    p.ledger.drop_item(id, [0.5, 64.0, 0.5], pile(item, count))
                }),
                1 => {
                    let ids: Vec<u64> = store.live_drops().map(|d| d.0).collect();
                    (!ids.is_empty())
                        .then(|| p.ledger.take_item(ids[rng.next(ids.len() as u64) as usize]))
                }
                2 | 3 => p.spend(pile(item, count)).then(|| {
                    p.ledger.chest_put(
                        chests[rng.next(2) as usize],
                        rng.next(4) as u8,
                        pile(item, count),
                    )
                }),
                _ => Some(p.ledger.chest_take(
                    chests[rng.next(2) as usize],
                    rng.next(4) as u8,
                    count,
                )),
            };
            if let Some(r) = req {
                asks.push((who, r));
            }
            // L'arbitre reçoit une demande au hasard parmi celles en route (ordre mêlé) ;
            // sur un mauvais réseau, certaines se perdent ou arrivent deux fois.
            if !asks.is_empty() && rng.next(3) != 0 {
                let i = rng.next(asks.len() as u64) as usize;
                let (from, r) = asks.swap_remove(i);
                let times = if lossy { rng.next(3) } else { 1 };
                for _ in 0..times {
                    for m in arbitrate(&mut store, &r, names[from]) {
                        if matches!(m, BuildMsg::Denied { .. }) {
                            wire.push((from, m));
                        } else {
                            log.push(m.clone());
                            for to in 0..names.len() {
                                wire.push((to, m.clone()));
                            }
                        }
                    }
                }
            }
            // Un coffre est parfois cassé : son contenu tombe.
            if step % 700 == 699 {
                let out = store.spill(chests[rng.next(2) as usize], || {
                    next_id += 1;
                    next_id
                });
                for m in out {
                    log.push(m.clone());
                    for to in 0..names.len() {
                        wire.push((to, m.clone()));
                    }
                }
            }
            // Remise des messages : au hasard, parfois perdus ou en double.
            for _ in 0..rng.next(8) {
                if wire.is_empty() {
                    break;
                }
                let i = rng.next(wire.len() as u64) as usize;
                let (to, m) = wire.swap_remove(i);
                let times = if lossy { rng.next(3) } else { 1 };
                for _ in 0..times {
                    players[to].hear(&m);
                }
            }
            // À tout instant : jamais plus d'objets qu'au départ.
            assert!(
                total(&players, &store, STONE) <= start[0],
                "pierre, pas {step}"
            );
            assert!(
                total(&players, &store, WOOD) <= start[1],
                "bois, pas {step}"
            );
        }
        // Tout ce qui est en route arrive.
        for (from, r) in asks.drain(..) {
            for m in arbitrate(&mut store, &r, names[from]) {
                if matches!(m, BuildMsg::Denied { .. }) {
                    wire.push((from, m));
                } else {
                    log.push(m.clone());
                    for to in 0..names.len() {
                        wire.push((to, m.clone()));
                    }
                }
            }
        }
        for (to, m) in wire.drain(..) {
            players[to].hear(&m);
        }
        let end = [
            total(&players, &store, STONE),
            total(&players, &store, WOOD),
        ];
        if lossy {
            assert!(end[0] <= start[0] && end[1] <= start[1]);
        } else {
            assert_eq!(end, start, "rien de perdu sans perte de message");
            assert!(players.iter().all(|p| p.ledger.pending() == 0));
        }
        // Un relais qui reprend le monde depuis les états publiés retrouve les mêmes objets.
        let mut copy = ItemStore::default();
        for m in &log {
            copy.absorb(m);
        }
        assert_eq!(copy.totals(), store.totals());
        // Et la sauvegarde (instantané) redonne le même monde.
        let mut saved = ItemStore::default();
        for m in store.snapshot() {
            saved.absorb(&m);
        }
        assert_eq!(saved.totals(), store.totals());
        assert_eq!(saved.chests, store.chests);
    }

    #[test]
    fn nothing_is_ever_duplicated_whatever_the_players_and_the_network_do() {
        for seed in 1..=40 {
            chaos(seed, false);
            chaos(seed * 7919, true);
        }
    }

    #[test]
    fn the_owner_can_restore_a_save_but_never_double_what_is_there() {
        let mut store = ItemStore::default();
        let chest = [4, 64, 4];
        let saved = [
            BuildMsg::Item {
                id: 1,
                at: [0.5, 64.0, 0.5],
                drop: Some(pile(STONE, 8)),
                to: "x".into(),
                seq: 3,
                got: Some(pile(STONE, 8)),
            },
            BuildMsg::Slot {
                pos: chest,
                slot: 2,
                pile: Some(pile(WOOD, 20)),
                by: "x".into(),
                seq: 4,
                got: Some(pile(WOOD, 20)),
            },
        ];
        for m in &saved {
            let out = store.seed(m).unwrap();
            // Remis sans crédit pour personne.
            match out {
                BuildMsg::Item { to, got, .. } => assert!(to.is_empty() && got.is_none()),
                BuildMsg::Slot { by, got, .. } => assert!(by.is_empty() && got.is_none()),
                _ => unreachable!(),
            }
        }
        let full = store.totals();
        // Remis une seconde fois : refusé.
        assert!(saved.iter().all(|m| store.seed(m).is_none()));
        // Un joueur ramasse l'objet et vide le coffre ; le propriétaire remet sa
        // sauvegarde : rien ne revient.
        store
            .apply(&BuildMsg::TakeItem { seq: 1, id: 1 }, "b")
            .unwrap();
        let take = BuildMsg::ChestTake {
            seq: 2,
            pos: chest,
            slot: 2,
            count: 20,
        };
        store.apply(&take, "b").unwrap();
        assert!(store.totals().is_empty());
        assert!(saved.iter().all(|m| store.seed(m).is_none()));
        assert!(store.totals().is_empty());
        assert_eq!(full[&STONE], 8);
    }

    #[test]
    fn the_oldest_drops_vanish_when_the_ground_is_full() {
        let mut store = ItemStore::default();
        for id in 0..MAX_DROPS as u64 + 10 {
            let req = BuildMsg::DropItem {
                seq: id as u32 + 1,
                id: 1_000_000 - id,
                pos: [0.0; 3],
                pile: pile(STONE, 1),
            };
            let out = store.apply(&req, "a").unwrap();
            assert_eq!(out.len(), if id < MAX_DROPS as u64 { 1 } else { 2 });
        }
        assert_eq!(store.live_drops().count(), MAX_DROPS);
        // Les premiers lâchés (les plus grands numéros ici) sont partis.
        assert!(store.drops[&1_000_000].is_none());
    }
}
