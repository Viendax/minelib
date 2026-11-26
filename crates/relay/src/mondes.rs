//! (v311) Registre des mondes de construction ouverts chez ce relais.
//!
//! Un monde ne disparaît plus de la liste quand son dernier joueur s'en va : il
//! **s'endort**. Le relais garde, dans `mondes.bin`, son nom, son créateur, son record de
//! joueurs, la date de sa dernière activité, et le pseudo de celui qui en garde le dernier
//! état : le dernier propriétaire, dont le jeu a enregistré le monde en partant. Quand ce
//! joueur, ou le créateur, rouvre le monde, il se réveille sous la même identité (mêmes OP,
//! mêmes bannis, même liste blanche). Avant la v350, le relais ne gardait pas ses blocs : tant
//! qu'aucun des deux n'était revenu, personne ne pouvait y entrer. (v350) Le relais en garde
//! maintenant une copie (`gardes`), d'où tout joueur qui peut y entrer le réveille.
//!
//! Le registre sert aussi à la liste des mondes (record, dernière activité), et à ne pas
//! laisser un compte ouvrir des mondes en rafale (`Store::may_open`).

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Ouvertures de partie ou de monde acceptées d'un compte pendant `OPENS_WINDOW`.
const OPENS: usize = 8;
const OPENS_WINDOW: Duration = Duration::from_secs(600);
/// Mondes (ouverts ou endormis) d'un même créateur. (v326) Au-delà, un nouveau monde ne
/// s'ouvre qu'en supprimant son plus ancien monde endormi, ce que le joueur confirme avec la
/// même protection que `/deleteworld` (avant, le plus ancien était oublié sans rien dire).
pub const PER_CREATOR: usize = minelib_net::proto::WORLDS_PER_CREATOR;
/// Mondes endormis montrés dans la liste, au plus.
pub const SLEEPING_SHOWN: usize = 100;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct World {
    /// Clé du serveur (`servers::key` : créateur et nom du monde).
    pub key: String,
    pub name: String,
    pub map: String,
    pub creator: String,
    /// Première ouverture et dernière activité (secondes depuis 1970).
    pub opened: u64,
    pub active: u64,
    /// Plus grand nombre de joueurs présents en même temps.
    pub peak: u32,
    /// Endormi depuis (rien : ouvert en ce moment).
    pub asleep: Option<u64>,
    /// Qui en garde le dernier état (le dernier propriétaire).
    pub keeper: String,
}

pub struct Store {
    path: PathBuf,
    worlds: HashMap<String, World>,
    /// Ouvertures récentes de chaque compte (en mémoire).
    opens: HashMap<String, VecDeque<Instant>>,
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl Store {
    pub fn open(path: PathBuf) -> Self {
        let worlds = std::fs::read(&path)
            .ok()
            .and_then(|b| postcard::from_bytes::<Vec<World>>(&b).ok())
            .unwrap_or_default()
            .into_iter()
            // (Le relais vient de démarrer : aucun monde n'est ouvert. Ceux qui l'étaient
            // quand il s'est arrêté dorment chez leur créateur.)
            .map(|mut w| {
                if w.asleep.is_none() {
                    w.asleep = Some(w.active);
                }
                (w.key.clone(), w)
            })
            .collect();
        Self {
            path,
            worlds,
            opens: HashMap::new(),
        }
    }

    fn save(&self) {
        let v: Vec<&World> = self.worlds.values().collect();
        if let Ok(b) = postcard::to_allocvec(&v) {
            let _ = crate::accounts::write_atomic(&self.path, &b);
        }
    }

    /// Ce compte peut-il ouvrir une partie ou un monde maintenant ? Sinon : dans combien
    /// de secondes. (Chaque ouverture acceptée compte.)
    #[cfg(test)]
    pub fn may_open(&mut self, pseudo: &str) -> Result<(), u64> {
        if let Some(wait) = self.open_wait(pseudo) {
            return Err(wait);
        }
        self.note_open(pseudo);
        Ok(())
    }

    /// (v326) Secondes à attendre avant une nouvelle ouverture (`None` : tout de suite), sans
    /// la compter : une ouverture qui demande d'abord une confirmation ne compte qu'une fois.
    pub fn open_wait(&mut self, pseudo: &str) -> Option<u64> {
        let q = self.opens.entry(pseudo.to_ascii_lowercase()).or_default();
        while q.front().is_some_and(|t| t.elapsed() >= OPENS_WINDOW) {
            q.pop_front();
        }
        (q.len() >= OPENS).then(|| {
            let wait = OPENS_WINDOW.saturating_sub(q.front().map_or(OPENS_WINDOW, |t| t.elapsed()));
            wait.as_secs().max(1)
        })
    }

    /// (v326) Une ouverture faite (voir `open_wait`).
    pub fn note_open(&mut self, pseudo: &str) {
        self.opens
            .entry(pseudo.to_ascii_lowercase())
            .or_default()
            .push_back(Instant::now());
    }

    /// (v326) `creator` a déjà `PER_CREATOR` mondes, ouverts ou endormis : un nouveau monde ne
    /// s'ouvre qu'en en supprimant un.
    pub fn full(&self, creator: &str) -> bool {
        self.worlds
            .values()
            .filter(|w| w.creator.eq_ignore_ascii_case(creator))
            .count()
            >= PER_CREATOR
    }

    /// (v326) Le monde endormi de `creator` le moins récemment actif (clé, nom) : celui qui
    /// fait place à un nouveau, si son créateur le confirme.
    pub fn oldest_asleep(&self, creator: &str) -> Option<(String, String)> {
        self.worlds
            .values()
            .filter(|w| w.asleep.is_some() && w.creator.eq_ignore_ascii_case(creator))
            .min_by(|a, b| a.active.cmp(&b.active).then(a.key.cmp(&b.key)))
            .map(|w| (w.key.clone(), w.name.clone()))
    }

    /// (v325) `pseudo` réveille le monde endormi `key` (son entrée dans la liste) : il en
    /// est le créateur, ou il en garde le dernier état. Rend son créateur. (La v311 le
    /// retrouvait au nom de la carte ouverte : rouvrir le monde solo réveillait le monde
    /// en ligne avec le contenu du solo.)
    pub fn wake(&self, key: &str, pseudo: &str) -> Option<String> {
        self.worlds
            .get(key)
            .filter(|w| w.asleep.is_some())
            .filter(|w| {
                w.creator.eq_ignore_ascii_case(pseudo) || w.keeper.eq_ignore_ascii_case(pseudo)
            })
            .map(|w| w.creator.clone())
    }

    /// (v325) Clé d'un monde neuf : `base` (créateur/carte), ou `base#2`, `base#3`… si un
    /// monde, endormi ou ouvert (`open`), la porte déjà. Rend aussi le numéro (1 : `base`).
    pub fn fresh_key(&self, base: &str, open: &[String]) -> (String, u32) {
        let taken = |k: &str| self.worlds.contains_key(k) || open.iter().any(|o| o == k);
        if !taken(base) {
            return (base.to_string(), 1);
        }
        let mut n = 2u32;
        loop {
            let k = format!("{base}#{n}");
            if !taken(&k) {
                return (k, n);
            }
            n += 1;
        }
    }

    /// Le monde `key` vient d'être ouvert (ou réveillé).
    pub fn opened(&mut self, key: &str, name: &str, map: &str, creator: &str, keeper: &str) {
        let t = now();
        let w = self.worlds.entry(key.to_string()).or_insert_with(|| World {
            key: key.to_string(),
            name: String::new(),
            map: map.to_string(),
            creator: creator.to_string(),
            opened: t,
            active: t,
            peak: 0,
            asleep: None,
            keeper: String::new(),
        });
        w.name = name.to_string();
        w.asleep = None;
        w.active = t;
        w.keeper = keeper.to_string();
        // (v326) Plus rien n'est oublié ici sans le dire : au-delà de `PER_CREATOR`, c'est
        // le joueur qui confirme la suppression d'un monde (`full`, `oldest_asleep`).
        self.save();
    }

    /// Des joueurs y sont en ce moment (relevé chaque minute, et à chaque arrivée).
    pub fn seen(&mut self, key: &str, players: u32) {
        if let Some(w) = self.worlds.get_mut(key) {
            if players > 0 {
                w.active = now();
            }
            if players > w.peak {
                w.peak = players;
                self.save();
            }
        }
    }

    /// Son dernier joueur est parti : `keeper` en garde le dernier état.
    pub fn asleep(&mut self, key: &str, keeper: &str) {
        if let Some(w) = self.worlds.get_mut(key) {
            let t = now();
            w.asleep = Some(t);
            w.active = t;
            w.keeper = keeper.to_string();
            self.save();
        }
    }

    /// Supprimé par son créateur.
    pub fn delete(&mut self, key: &str) {
        if self.worlds.remove(key).is_some() {
            self.save();
        }
    }

    /// (v349) Oubliés pour faire de la place (`place`) : une seule écriture.
    pub fn delete_many(&mut self, keys: &[String]) {
        let before = self.worlds.len();
        for k in keys {
            self.worlds.remove(k);
        }
        if self.worlds.len() != before {
            self.save();
        }
    }

    /// (v349) Tous les mondes, ouverts ou endormis.
    pub fn all(&self) -> impl Iterator<Item = &World> {
        self.worlds.values()
    }

    /// (v349) Ce que son entrée prend dans `mondes.bin` (octets).
    pub fn bytes_of(&self, key: &str) -> u64 {
        self.worlds
            .get(key)
            .and_then(|w| postcard::to_allocvec(w).ok())
            .map_or(0, |b| b.len() as u64)
    }

    /// (v349) Essais : ce monde a eu son dernier joueur il y a `days` jours (`asleep` :
    /// endormi depuis ; sinon noté ouvert).
    #[cfg(test)]
    pub fn age(&mut self, key: &str, days: u64, asleep: bool) {
        if let Some(w) = self.worlds.get_mut(key) {
            w.active = now().saturating_sub(days * 86_400);
            w.asleep = asleep.then_some(w.active);
        }
    }

    pub fn get(&self, key: &str) -> Option<&World> {
        self.worlds.get(key)
    }

    /// Mondes endormis, ceux qui ont eu le plus de joueurs d'abord.
    pub fn sleeping(&self) -> Vec<&World> {
        let mut v: Vec<&World> = self
            .worlds
            .values()
            .filter(|w| w.asleep.is_some())
            .collect();
        v.sort_by(|a, b| {
            b.peak
                .cmp(&a.peak)
                .then(b.active.cmp(&a.active))
                .then(a.key.cmp(&b.key))
        });
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        let dir = std::env::temp_dir().join(format!("minelib-mondes-{}", rand::random::<u32>()));
        std::fs::create_dir_all(&dir).unwrap();
        Store::open(dir.join("mondes.bin"))
    }

    /// Un monde s'endort avec son record et son gardien, se relit après un redémarrage du
    /// relais, et se réveille sous son identité pour son créateur ou son gardien seulement.
    #[test]
    fn a_world_falls_asleep_and_wakes_under_its_own_identity() {
        let mut s = store();
        s.opened("ana/ile", "L'île", "ile", "Ana", "Ana");
        s.seen("ana/ile", 3);
        s.seen("ana/ile", 7);
        s.seen("ana/ile", 2);
        assert_eq!(s.get("ana/ile").unwrap().peak, 7);
        assert!(s.sleeping().is_empty() && s.wake("ana/ile", "Ana").is_none());
        s.asleep("ana/ile", "Bob");
        let path = s.path.clone();
        let s = Store::open(path);
        let w = s.get("ana/ile").unwrap();
        assert!(w.asleep.is_some() && w.keeper == "Bob" && w.peak == 7 && w.name == "L'île");
        assert_eq!(s.wake("ana/ile", "bob"), Some("Ana".into()));
        assert_eq!(s.wake("ana/ile", "ANA"), Some("Ana".into()));
        assert_eq!(s.wake("ana/ile", "Zoe"), None);
        assert_eq!(s.wake("ana/autre", "Bob"), None);
        // (v325) Un monde neuf ne reprend jamais la clé d'un autre, endormi ou ouvert.
        assert_eq!(s.fresh_key("ana/ile", &[]), ("ana/ile#2".into(), 2));
        assert_eq!(s.fresh_key("zoe/ile", &[]), ("zoe/ile".into(), 1));
        assert_eq!(
            s.fresh_key("zoe/ile", &["zoe/ile".into()]),
            ("zoe/ile#2".into(), 2)
        );
    }

    /// Un monde ouvert quand le relais s'arrête dort à son redémarrage ; supprimé, il
    /// n'existe plus ; les endormis sont rangés par record.
    #[test]
    fn worlds_open_when_the_relay_stops_are_asleep_when_it_comes_back() {
        let mut s = store();
        s.opened("ana/a", "A", "a", "Ana", "Ana");
        s.opened("ana/b", "B", "b", "Ana", "Ana");
        s.seen("ana/b", 9);
        s.seen("ana/a", 4);
        let mut s = Store::open(s.path.clone());
        let names: Vec<&str> = s.sleeping().iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, ["B", "A"]);
        s.delete("ana/b");
        let s = Store::open(s.path.clone());
        assert!(s.get("ana/b").is_none() && s.sleeping().len() == 1);
    }

    /// Pas d'ouvertures en rafale : huit en dix minutes par compte, puis il faut attendre.
    #[test]
    fn an_account_cannot_open_worlds_in_a_burst() {
        let mut s = store();
        for _ in 0..OPENS {
            assert!(s.may_open("Spam").is_ok());
        }
        let wait = s.may_open("spam").unwrap_err();
        assert!(wait > 0 && wait <= OPENS_WINDOW.as_secs());
        assert!(s.may_open("Autre").is_ok());
    }

    /// De bout en bout, avec de vrais jeux : un monde ouvert par Ana, où Bob entre, passe
    /// à Bob quand Ana s'en va, s'endort quand Bob s'en va (Bob en garde le dernier état),
    /// reste dans la liste avec son record ; (v325) Ana qui rouvre son monde solo en fait un
    /// autre serveur ; il se réveille par sa clé quand Bob le réveille, toujours celui d'Ana
    /// (pas pour Zoe) ; Bob ne peut pas le supprimer, Ana oui, et tout le monde en sort.
    #[test]
    fn a_world_sleeps_wakes_for_its_keeper_and_only_its_creator_deletes_it() {
        use crate::{start, Config};
        use minelib_net::client::{Client, Cmd, Event};
        use minelib_net::proto;
        let dir = std::env::temp_dir().join(format!("minelib-monde-{}", rand::random::<u32>()));
        let h = start(Config {
            dir,
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "essai".into(),
        })
        .unwrap();
        let wait = |c: &Client, pred: &dyn Fn(&Event) -> bool| -> Event {
            let t = Instant::now();
            loop {
                if let Some(e) = c.poll() {
                    if pred(&e) {
                        return e;
                    }
                }
                assert!(t.elapsed() < Duration::from_secs(30), "délai dépassé");
                std::thread::sleep(Duration::from_millis(2));
            }
        };
        let login = |pseudo: &str| {
            let c = Client::spawn(h.addr.to_string(), Some(h.public));
            c.send(Cmd::Register {
                pseudo: pseudo.into(),
                password: "motdepasse".into(),
            });
            wait(&c, &|e| matches!(e, Event::LoggedIn { .. }));
            c
        };
        let host = |c: &Client| -> u64 {
            c.send(Cmd::Host {
                kind: 1,
                name: "L'île".into(),
                map: "ile".into(),
                map_sig: [3; 32],
                max_players: 0,
                seed: 0,
                terrain: 0,
            });
            match wait(c, &|e| matches!(e, Event::Hosted(_))) {
                Event::Hosted(r) => r,
                _ => unreachable!(),
            }
        };
        let list = |c: &Client| -> Vec<proto::ServerEntry> {
            c.send(Cmd::Servers(1));
            match wait(c, &|e| matches!(e, Event::Servers(_))) {
                Event::Servers(l) => l,
                _ => unreachable!(),
            }
        };
        let (ana, bob) = (login("AnaMonde"), login("BobMonde"));
        let rid = host(&ana);
        ana.send(Cmd::ServerAdmin {
            action: proto::SERVER_WL_OFF,
            pseudo: String::new(),
            reason: String::new(),
        });
        wait(&ana, &|e| matches!(e, Event::ServerAdmin(_)));
        bob.send(Cmd::Join(rid));
        wait(&bob, &|e| matches!(e, Event::Joined { .. }));
        // Ouvert, deux joueurs : en tête de liste, record 2.
        let l = list(&bob);
        assert_eq!(
            (l.len(), l[0].players, l[0].peak, l[0].asleep),
            (1, 2, 2, false)
        );
        assert!(l[0].opened > 0 && l[0].active >= l[0].opened);
        // Ana s'en va : Bob devient le propriétaire. Bob s'en va : le monde s'endort.
        ana.send(Cmd::Leave);
        wait(&bob, &|e| matches!(e, Event::Promoted { .. }));
        bob.send(Cmd::Leave);
        let t = Instant::now();
        while !h.state.lock().unwrap().rooms.is_empty() {
            assert!(t.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(5));
        }
        let l = list(&ana);
        assert_eq!(l.len(), 1, "le monde endormi reste dans la liste");
        assert!(l[0].asleep && l[0].id == 0 && l[0].peak == 2 && l[0].players == 0);
        assert_eq!(
            (l[0].keeper.as_str(), l[0].host.as_str()),
            ("BobMonde", "AnaMonde")
        );
        assert_eq!(l[0].world, "anamonde/ile");
        // Un inconnu qui ouvre un monde du même nom n'a pas celui-là.
        let zoe = login("ZoeMonde");
        let other = host(&zoe);
        assert_eq!(h.state.lock().unwrap().rooms[&other].creator, "ZoeMonde");
        zoe.send(Cmd::Leave);
        // (v325) Ana rouvre son monde solo : c'est un autre serveur, l'endormi n'y est pour
        // rien (il garde son contenu à lui).
        let again = host(&ana);
        {
            let st = h.state.lock().unwrap();
            let r = &st.rooms[&again];
            assert_eq!(
                (r.server.as_str(), r.name.as_str()),
                ("anamonde/ile#2", "L'île 2")
            );
            assert!(st.worlds.get("anamonde/ile").unwrap().asleep.is_some());
        }
        ana.send(Cmd::Leave);
        // Zoe ne réveille pas un monde qui n'est pas le sien ; Bob, qui en garde le dernier
        // état, le réveille par sa clé.
        let wake = |c: &Client, world: &str| {
            c.send(Cmd::WakeWorld {
                name: "L'île".into(),
                map: "ile".into(),
                map_sig: [5; 32],
                world: world.into(),
                seed: 0,
                terrain: 0,
            });
        };
        wake(&zoe, "anamonde/ile");
        match wait(&zoe, &|e| {
            matches!(e, Event::HostRefused(_) | Event::Hosted(_))
        }) {
            Event::HostRefused(m) => assert!(m.contains("réveiller"), "{m}"),
            e => panic!("{e:?}"),
        }
        wake(&bob, "anamonde/ile");
        let rid = match wait(&bob, &|e| matches!(e, Event::Hosted(_))) {
            Event::Hosted(r) => r,
            _ => unreachable!(),
        };
        {
            let st = h.state.lock().unwrap();
            let r = &st.rooms[&rid];
            assert_eq!(
                (r.creator.as_str(), r.server.as_str()),
                ("AnaMonde", "anamonde/ile")
            );
            assert!(st.worlds.get("anamonde/ile").unwrap().asleep.is_none());
            // (Réveillé avec ses réglages : la liste blanche est restée ouverte.)
            assert!(!st.servers.whitelist_on("anamonde/ile"));
        }
        // Bob n'en est pas le créateur : il ne le supprime pas.
        bob.send(Cmd::DeleteWorld(None));
        match wait(&bob, &|e| matches!(e, Event::Error(_))) {
            Event::Error(m) => assert!(m.contains("créateur"), "{m}"),
            _ => unreachable!(),
        }
        // Ana y revient et le supprime : Bob en sort, le monde n'est plus nulle part.
        ana.send(Cmd::Join(rid));
        wait(&ana, &|e| matches!(e, Event::Joined { .. }));
        ana.send(Cmd::DeleteWorld(None));
        match wait(&bob, &|e| matches!(e, Event::Kicked(_))) {
            Event::Kicked(m) => assert!(m.contains("supprimé"), "{m}"),
            _ => unreachable!(),
        }
        wait(&ana, &|e| matches!(e, Event::Kicked(_)));
        let st = h.state.lock().unwrap();
        assert!(st.rooms.values().all(|r| r.kind != 1));
        assert!(st.worlds.get("anamonde/ile").is_none());
        assert!(st.servers.ops("anamonde/ile").is_empty());
        drop(st);
        assert!(list(&zoe).iter().all(|e| e.world != "anamonde/ile"));
    }

    /// (v325) Ses propres mondes en tête de la liste, ouverts ou endormis, le plus récemment
    /// actif d'abord ; ses mondes endormis (créés ou gardés) y sont même au-delà de ceux
    /// qu'on montre ; pour un autre, rien de tout ça.
    #[test]
    fn own_worlds_head_the_list_most_recently_active_first() {
        use crate::{start, Config};
        let dir = std::env::temp_dir().join(format!("minelib-mondes3-{}", rand::random::<u32>()));
        let h = start(Config {
            dir,
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "essai".into(),
        })
        .unwrap();
        let mut st = h.state.lock().unwrap();
        // D'autres mondes endormis, plus qu'on n'en montre, tous avec un meilleur record.
        for i in 0..SLEEPING_SHOWN + 5 {
            let key = format!("zed{i}/m");
            st.worlds.opened(&key, "M", "m", &format!("Zed{i}"), "x");
            st.worlds.seen(&key, 9);
            st.worlds.asleep(&key, "x");
        }
        // Deux mondes d'Ana, endormis, sans record : un ancien et un récent ; un monde que
        // garde Ana (créé par Bob) ; et Ana a un monde ouvert, moins récent que le récent.
        for (key, active) in [("ana/vieux", 1_000), ("ana/neuf", 3_000)] {
            st.worlds.opened(key, key, key, "Ana", "Ana");
            st.worlds.asleep(key, "Ana");
            st.worlds.worlds.get_mut(key).unwrap().active = active;
        }
        st.worlds.opened("bob/garde", "G", "garde", "Bob", "Bob");
        st.worlds.asleep("bob/garde", "Ana");
        drop(st);
        let c = minelib_net::client::Client::spawn(h.addr.to_string(), Some(h.public));
        c.send(minelib_net::client::Cmd::Register {
            pseudo: "Ana".into(),
            password: "motdepasse".into(),
        });
        let t = Instant::now();
        loop {
            if let Some(minelib_net::client::Event::LoggedIn { .. }) = c.poll() {
                break;
            }
            assert!(t.elapsed() < Duration::from_secs(30), "délai dépassé");
            std::thread::sleep(Duration::from_millis(2));
        }
        c.send(minelib_net::client::Cmd::Host {
            kind: 1,
            name: "Ouvert".into(),
            map: "ouvert".into(),
            map_sig: [1; 32],
            max_players: 0,
            seed: 0,
            terrain: 0,
        });
        let t = Instant::now();
        loop {
            if let Some(minelib_net::client::Event::Hosted(_)) = c.poll() {
                break;
            }
            assert!(t.elapsed() < Duration::from_secs(30), "délai dépassé");
            std::thread::sleep(Duration::from_millis(2));
        }
        let mut st = h.state.lock().unwrap();
        st.worlds.worlds.get_mut("ana/ouvert").unwrap().active = 2_000;
        let l = crate::local_servers(&st, 1, Some("ANA"));
        let keys: Vec<&str> = l.iter().map(|e| e.world.as_str()).collect();
        assert_eq!(&keys[..3], ["ana/neuf", "ana/ouvert", "ana/vieux"]);
        assert!(l[0].creator == "Ana" && l[0].asleep && !l[1].asleep && l[1].players == 1);
        assert!(
            keys.contains(&"bob/garde"),
            "le monde qu'Ana garde y est, même au-delà"
        );
        assert_eq!(
            l.iter().filter(|e| e.creator.starts_with("Zed")).count(),
            SLEEPING_SHOWN
        );
        // Pour un autre : les mondes d'Ana à leur place ordinaire (son monde ouvert d'abord,
        // ses endormis sans record au-delà du nombre montré).
        let l = crate::local_servers(&st, 1, Some("Zoe"));
        assert_eq!(
            (l.len(), l[0].world.as_str()),
            (SLEEPING_SHOWN + 1, "ana/ouvert")
        );
        assert!(l[1..]
            .iter()
            .all(|e| e.creator.starts_with("Zed") && e.asleep));
    }

    /// Un créateur qui a la double authentification ne supprime son monde qu'avec un code
    /// valable : sans code ou avec un faux, le relais en redemande un et le monde reste.
    #[test]
    fn deleting_a_world_takes_a_two_factor_code_when_the_account_has_one() {
        use crate::{start, Config};
        use minelib_net::client::{Client, Cmd, Event};
        use minelib_net::totp;
        let dir = std::env::temp_dir().join(format!("minelib-monde2-{}", rand::random::<u32>()));
        let h = start(Config {
            dir,
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "essai".into(),
        })
        .unwrap();
        let wait = |c: &Client, pred: &dyn Fn(&Event) -> bool| -> Event {
            let t = Instant::now();
            loop {
                if let Some(e) = c.poll() {
                    if pred(&e) {
                        return e;
                    }
                }
                assert!(t.elapsed() < Duration::from_secs(30), "délai dépassé");
                std::thread::sleep(Duration::from_millis(2));
            }
        };
        let ana = Client::spawn(h.addr.to_string(), Some(h.public));
        ana.send(Cmd::Register {
            pseudo: "AnaCode".into(),
            password: "motdepasse".into(),
        });
        wait(&ana, &|e| matches!(e, Event::LoggedIn { .. }));
        ana.send(Cmd::TotpBegin);
        let secret = match wait(&ana, &|e| matches!(e, Event::TotpSecret { .. })) {
            Event::TotpSecret { secret, .. } => secret,
            _ => unreachable!(),
        };
        // (Le code de la période d'avant active la double authentification ; ceux de la
        // période en cours et de la suivante restent à servir.)
        let step = totp::now_step();
        ana.send(Cmd::TotpConfirm(totp::code(&secret, step - 1)));
        wait(&ana, &|e| matches!(e, Event::TotpOn));
        ana.send(Cmd::Host {
            kind: 1,
            name: "Coffre".into(),
            map: "coffre".into(),
            map_sig: [4; 32],
            max_players: 0,
            seed: 0,
            terrain: 0,
        });
        wait(&ana, &|e| matches!(e, Event::Hosted(_)));
        ana.send(Cmd::DeleteWorld(None));
        wait(&ana, &|e| matches!(e, Event::DeleteWorldCode));
        let wrong = (totp::code(&secret, step + 1) + 1) % 1_000_000;
        ana.send(Cmd::DeleteWorld(Some(wrong)));
        wait(&ana, &|e| matches!(e, Event::DeleteWorldCode));
        assert!(h
            .state
            .lock()
            .unwrap()
            .worlds
            .get("anacode/coffre")
            .is_some());
        ana.send(Cmd::DeleteWorld(Some(totp::code(&secret, step + 1))));
        wait(&ana, &|e| matches!(e, Event::Kicked(_)));
        let st = h.state.lock().unwrap();
        assert!(st.worlds.get("anacode/coffre").is_none() && st.rooms.is_empty());
    }

    /// (v326) À vingt mondes, un nouveau monde ne s'ouvre qu'en supprimant le plus ancien
    /// endormi, confirmé par le joueur, avec un code valable si son compte a la double
    /// authentification : rien n'est supprimé avant. Vingt mondes ouverts : refusé.
    #[test]
    fn at_the_limit_a_new_world_replaces_the_oldest_only_once_confirmed() {
        use crate::{start, Config};
        use minelib_net::client::{Client, Cmd, Event};
        use minelib_net::totp;
        let dir = std::env::temp_dir().join(format!("minelib-monde4-{}", rand::random::<u32>()));
        let h = start(Config {
            dir,
            bind: "127.0.0.1:0".into(),
            pow_bits: 4,
            name: "essai".into(),
        })
        .unwrap();
        let wait = |c: &Client, pred: &dyn Fn(&Event) -> bool| -> Event {
            let t = Instant::now();
            loop {
                if let Some(e) = c.poll() {
                    if pred(&e) {
                        return e;
                    }
                }
                assert!(t.elapsed() < Duration::from_secs(30), "délai dépassé");
                std::thread::sleep(Duration::from_millis(2));
            }
        };
        let login = |pseudo: &str| {
            let c = Client::spawn(h.addr.to_string(), Some(h.public));
            c.send(Cmd::Register {
                pseudo: pseudo.into(),
                password: "motdepasse".into(),
            });
            wait(&c, &|e| matches!(e, Event::LoggedIn { .. }));
            c
        };
        let fill = |creator: &str, asleep: bool| {
            let mut st = h.state.lock().unwrap();
            for i in 0..PER_CREATOR {
                let key = format!("{}/m{i}", creator.to_ascii_lowercase());
                st.worlds
                    .opened(&key, &format!("Monde {i}"), &format!("m{i}"), creator, creator);
                if asleep {
                    st.worlds.asleep(&key, creator);
                }
                st.worlds.worlds.get_mut(&key).unwrap().active = 1_000 + i as u64;
            }
        };
        let host = |c: &Client, replace: &str, code: Option<u32>| -> Event {
            if replace.is_empty() {
                c.send(Cmd::Host {
                    kind: 1,
                    name: "Neuf".into(),
                    map: "neuf".into(),
                    map_sig: [7; 32],
                    max_players: 0,
                    seed: 0,
                    terrain: 0,
                });
            } else {
                c.send(Cmd::HostReplacing {
                    name: "Neuf".into(),
                    map: "neuf".into(),
                    map_sig: [7; 32],
                    replace: replace.into(),
                    code,
                    seed: 0,
                    terrain: 0,
                });
            }
            wait(c, &|e| {
                matches!(
                    e,
                    Event::WorldLimit { .. } | Event::Hosted(_) | Event::HostRefused(_)
                )
            })
        };
        let ana = login("AnaPlein");
        ana.send(Cmd::TotpBegin);
        let secret = match wait(&ana, &|e| matches!(e, Event::TotpSecret { .. })) {
            Event::TotpSecret { secret, .. } => secret,
            _ => unreachable!(),
        };
        let step = totp::now_step();
        ana.send(Cmd::TotpConfirm(totp::code(&secret, step - 1)));
        wait(&ana, &|e| matches!(e, Event::TotpOn));
        fill("AnaPlein", true);
        // Sans confirmation : le relais dit lequel ferait place, et qu'il faut un code.
        match host(&ana, "", None) {
            Event::WorldLimit { world, name, totp } => {
                assert_eq!(
                    (world.as_str(), name.as_str(), totp),
                    ("anaplein/m0", "Monde 0", true)
                );
            }
            e => panic!("{e:?}"),
        }
        // Confirmé sans code, avec un faux code, ou pour un autre monde : rien.
        assert!(matches!(host(&ana, "anaplein/m0", None), Event::WorldLimit { .. }));
        let wrong = (totp::code(&secret, step + 1) + 1) % 1_000_000;
        let e = host(&ana, "anaplein/m0", Some(wrong));
        assert!(matches!(e, Event::WorldLimit { .. }), "{e:?}");
        let good = totp::code(&secret, step + 1);
        assert!(matches!(host(&ana, "anaplein/m5", Some(good)), Event::WorldLimit { .. }));
        {
            let st = h.state.lock().unwrap();
            assert_eq!(st.worlds.sleeping().len(), PER_CREATOR);
            assert!(st.rooms.is_empty() && st.worlds.get("anaplein/neuf").is_none());
        }
        // Confirmé, avec un code valable : le plus ancien est supprimé, le nouveau s'ouvre.
        assert!(matches!(host(&ana, "anaplein/m0", Some(good)), Event::Hosted(_)));
        {
            let st = h.state.lock().unwrap();
            assert!(st.worlds.get("anaplein/m0").is_none());
            assert!(st.worlds.get("anaplein/m1").is_some());
            assert!(st.worlds.get("anaplein/neuf").is_some() && st.worlds.full("AnaPlein"));
        }
        // Sans double authentification : la confirmation suffit. Vingt mondes ouverts : refusé.
        let bob = login("BobPlein");
        fill("BobPlein", false);
        match host(&bob, "", None) {
            Event::HostRefused(m) => assert!(m.contains("ouverts"), "{m}"),
            e => panic!("{e:?}"),
        }
        h.state.lock().unwrap().worlds.asleep("bobplein/m3", "BobPlein");
        match host(&bob, "", None) {
            Event::WorldLimit { world, totp, .. } => {
                assert_eq!((world.as_str(), totp), ("bobplein/m3", false));
            }
            e => panic!("{e:?}"),
        }
        assert!(matches!(host(&bob, "bobplein/m3", None), Event::Hosted(_)));
        assert!(h.state.lock().unwrap().worlds.get("bobplein/m3").is_none());
    }

    /// (v326) Le registre n'oublie plus de monde sans le dire : à vingt mondes, il le dit
    /// (`full`) et désigne le plus ancien endormi, celui qui ferait place ; une ouverture
    /// qui attend une confirmation ne compte pas comme une ouverture.
    #[test]
    fn at_twenty_worlds_the_registry_names_the_oldest_and_forgets_nothing() {
        let mut s = store();
        for i in 0..PER_CREATOR - 1 {
            let key = format!("ana/m{i}");
            s.opened(&key, "M", &format!("m{i}"), "Ana", "Ana");
            s.asleep(&key, "Ana");
            s.worlds.get_mut(&key).unwrap().active = 1_000 + i as u64;
        }
        assert!(!s.full("ana"));
        s.opened("ana/ouvert", "O", "ouvert", "Ana", "Ana");
        assert!(s.full("ANA") && !s.full("Bob"));
        assert_eq!(s.oldest_asleep("ana"), Some(("ana/m0".into(), "M".into())));
        for i in 0..5 {
            let key = format!("ana/plus{i}");
            s.opened(&key, "P", "plus", "Ana", "Ana");
            s.asleep(&key, "Ana");
        }
        assert_eq!(s.sleeping().len(), PER_CREATOR + 4, "rien d'oublié sans le dire");
        assert_eq!(s.oldest_asleep("Zoe"), None);
        for _ in 0..OPENS {
            assert!(s.open_wait("Spam").is_none());
            s.note_open("Spam");
        }
        assert!(s.open_wait("spam").is_some() && s.open_wait("Autre").is_none());
    }
}
