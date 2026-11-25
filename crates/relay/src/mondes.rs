//! (v311) Registre des mondes de construction ouverts chez ce relais.
//!
//! Un monde ne disparaît plus de la liste quand son dernier joueur s'en va : il
//! **s'endort**. Le relais garde, dans `mondes.bin`, son nom, son créateur, son record de
//! joueurs, la date de sa dernière activité, et le pseudo de celui qui en garde le dernier
//! état : le dernier propriétaire, dont le jeu a enregistré le monde en partant (le relais,
//! lui, ne garde pas les blocs d'un monde). Quand ce joueur, ou le créateur, rouvre le
//! monde, il se réveille sous la même identité (mêmes OP, mêmes bannis, même liste
//! blanche). Tant qu'aucun des deux n'est revenu, personne ne peut y entrer : il n'y a
//! nulle part d'où prendre ses blocs.
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
/// Mondes endormis gardés pour un même créateur (les plus anciens sont oubliés au-delà).
const PER_CREATOR: usize = 20;
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
    pub fn may_open(&mut self, pseudo: &str) -> Result<(), u64> {
        let q = self.opens.entry(pseudo.to_ascii_lowercase()).or_default();
        while q.front().is_some_and(|t| t.elapsed() >= OPENS_WINDOW) {
            q.pop_front();
        }
        if q.len() >= OPENS {
            let wait = OPENS_WINDOW.saturating_sub(q.front().map_or(OPENS_WINDOW, |t| t.elapsed()));
            return Err(wait.as_secs().max(1));
        }
        q.push_back(Instant::now());
        Ok(())
    }

    /// Le monde endormi que `pseudo` réveille en ouvrant un monde nommé `map` : le sien
    /// (il en est le créateur), ou celui dont il garde le dernier état. Rend son créateur
    /// et sa clé.
    pub fn wakes(&self, pseudo: &str, map: &str) -> Option<(String, String)> {
        self.worlds
            .values()
            .filter(|w| w.asleep.is_some() && w.map == map)
            .filter(|w| {
                w.creator.eq_ignore_ascii_case(pseudo) || w.keeper.eq_ignore_ascii_case(pseudo)
            })
            // (Le sien d'abord, sinon le plus récemment actif.)
            .max_by_key(|w| (w.creator.eq_ignore_ascii_case(pseudo), w.active))
            .map(|w| (w.creator.clone(), w.key.clone()))
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
        // Pas plus de `PER_CREATOR` mondes endormis par créateur.
        let mut mine: Vec<(u64, String)> = self
            .worlds
            .values()
            .filter(|x| x.asleep.is_some() && x.creator.eq_ignore_ascii_case(creator))
            .map(|x| (x.active, x.key.clone()))
            .collect();
        mine.sort();
        let extra = mine.len().saturating_sub(PER_CREATOR);
        for (_, k) in mine.into_iter().take(extra) {
            self.worlds.remove(&k);
        }
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

    pub fn get(&self, key: &str) -> Option<&World> {
        self.worlds.get(key)
    }

    /// Mondes endormis, ceux qui ont eu le plus de joueurs d'abord.
    pub fn sleeping(&self) -> Vec<&World> {
        let mut v: Vec<&World> = self.worlds.values().filter(|w| w.asleep.is_some()).collect();
        v.sort_by(|a, b| b.peak.cmp(&a.peak).then(b.active.cmp(&a.active)).then(a.key.cmp(&b.key)));
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
        assert!(s.sleeping().is_empty() && s.wakes("Ana", "ile").is_none());
        s.asleep("ana/ile", "Bob");
        let path = s.path.clone();
        let s = Store::open(path);
        let w = s.get("ana/ile").unwrap();
        assert!(w.asleep.is_some() && w.keeper == "Bob" && w.peak == 7 && w.name == "L'île");
        assert_eq!(s.wakes("bob", "ile"), Some(("Ana".into(), "ana/ile".into())));
        assert_eq!(s.wakes("ANA", "ile"), Some(("Ana".into(), "ana/ile".into())));
        assert_eq!(s.wakes("Zoe", "ile"), None);
        assert_eq!(s.wakes("Bob", "autre"), None);
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
    /// reste dans la liste avec son record, se réveille quand Bob le rouvre, toujours
    /// celui d'Ana ; Bob ne peut pas le supprimer, Ana oui, et tout le monde en sort.
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
        assert_eq!((l.len(), l[0].players, l[0].peak, l[0].asleep), (1, 2, 2, false));
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
        assert_eq!((l[0].keeper.as_str(), l[0].host.as_str()), ("BobMonde", "AnaMonde"));
        // Un inconnu qui ouvre un monde du même nom n'a pas celui-là ; Bob, si.
        let zoe = login("ZoeMonde");
        let other = host(&zoe);
        assert_eq!(h.state.lock().unwrap().rooms[&other].creator, "ZoeMonde");
        zoe.send(Cmd::Leave);
        let rid = host(&bob);
        {
            let st = h.state.lock().unwrap();
            let r = &st.rooms[&rid];
            assert_eq!((r.creator.as_str(), r.server.as_str()), ("AnaMonde", "anamonde/ile"));
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
        assert!(list(&zoe).iter().all(|e| e.map != "ile" || e.host == "ZoeMonde"));
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
        });
        wait(&ana, &|e| matches!(e, Event::Hosted(_)));
        ana.send(Cmd::DeleteWorld(None));
        wait(&ana, &|e| matches!(e, Event::DeleteWorldCode));
        let wrong = (totp::code(&secret, step + 1) + 1) % 1_000_000;
        ana.send(Cmd::DeleteWorld(Some(wrong)));
        wait(&ana, &|e| matches!(e, Event::DeleteWorldCode));
        assert!(h.state.lock().unwrap().worlds.get("anacode/coffre").is_some());
        ana.send(Cmd::DeleteWorld(Some(totp::code(&secret, step + 1))));
        wait(&ana, &|e| matches!(e, Event::Kicked(_)));
        let st = h.state.lock().unwrap();
        assert!(st.worlds.get("anacode/coffre").is_none() && st.rooms.is_empty());
    }

    /// Un créateur ne garde pas plus de vingt mondes endormis.
    #[test]
    fn a_creator_keeps_a_bounded_number_of_sleeping_worlds() {
        let mut s = store();
        for i in 0..PER_CREATOR + 5 {
            let key = format!("ana/m{i}");
            s.opened(&key, "M", &format!("m{i}"), "Ana", "Ana");
            s.asleep(&key, "Ana");
        }
        s.opened("ana/dernier", "D", "dernier", "Ana", "Ana");
        assert_eq!(s.sleeping().len(), PER_CREATOR);
    }
}
