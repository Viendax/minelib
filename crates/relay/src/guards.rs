//! Gardiens des mondes (règles : `minelib_net::guard`).
//!
//! Côté relais du monde : la liste des gardiens de chaque monde qui a un relais de secours
//! (lui, des témoins choisis parmi les connexions de garant, même d'autres mondes), les
//! battements, les baux, la pause quand il n'entend plus assez de gardiens, et l'arrêt
//! propre. Côté relais de secours : ses propres baux et son vote, les votes des témoins
//! (qui y entrent avec leur certificat), et la reprise.

use std::collections::{HashMap, HashSet};

use minelib_net::guard::{self, Keeper, Warden};
use minelib_net::proto::{ErrorCode, Request, Response};
use minelib_net::transport;

use super::{err, now_secs, same_machine, Conn, Session, State};

/// Gardiens d'un monde, vus de son relais du monde.
pub(crate) struct RoomGuards {
    pub warden: Warden,
    /// Connexion de chaque gardien (pseudo en minuscules).
    pub conns: HashMap<String, u64>,
    /// Gardiens retirés parce qu'ils ne répondaient plus (pseudos en minuscules) : pas
    /// repris avant cette heure. (Un témoin qui a voté ne redevient pas gardien de ce monde
    /// de lui-même : son jeu garde ses votes, même d'une session à l'autre.)
    pub cooldown: HashMap<String, u64>,
    /// En pause depuis : sans issue au bout de `RELEASE_MS`, le monde est confié au relais
    /// de secours.
    pub paused_since: Option<u64>,
    /// Le monde était-il servi au dernier passage (les joueurs savent s'il est en pause) ?
    pub serving: bool,
    pub last_beat: u64,
    pub last_heartbeat: u64,
    /// Dernière recherche d'un témoin de plus (toutes les connexions passées en revue : au
    /// plus toutes les `SCAN_MS`).
    pub last_scan: u64,
}

/// Recherche d'un témoin de plus, au plus à ce rythme (ms) par monde.
const SCAN_MS: u64 = 2000;
/// Un gardien retiré parce qu'il ne répondait plus n'est pas repris avant ce temps (ms).
const COOLDOWN_MS: u64 = 60_000;

/// Gardiens retirés de ce monde récemment (pas repris tout de suite).
pub(crate) fn cooling(st: &State, rid: u64) -> HashSet<String> {
    let now = now_ms(st);
    st.rooms
        .get(&rid)
        .and_then(|r| r.guards.as_ref())
        .map(|g| {
            g.cooldown
                .iter()
                .filter(|(_, &until)| until > now)
                .map(|(p, _)| p.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Heure des gardiens : millisecondes sur l'horloge de ce relais (elle ne recule pas).
pub(crate) fn now_ms(st: &State) -> u64 {
    st.guard_epoch.elapsed().as_millis() as u64
}

/// Le monde est-il en pause : son relais du monde n'entend plus assez de gardiens (ou il
/// s'est arrêté proprement) ? Rien n'y change alors : ni blocs, ni objets, ni réglages.
pub(crate) fn frozen(st: &State, rid: u64) -> bool {
    let now = now_ms(st);
    st.rooms
        .get(&rid)
        .and_then(|r| r.guards.as_ref())
        .is_some_and(|g| !g.warden.serving(now))
}

/// Essais : le réseau entre ce relais et ce gardien est coupé (ni battement ni bail).
fn cut(st: &State, pseudo: &str) -> bool {
    st.guard_isolated || st.guard_cut.contains(&pseudo.to_ascii_lowercase())
}

/// Entretien (toutes les 50 ms) : listes de gardiens, battements, pause.
pub(crate) fn upkeep(st: &mut State) {
    let rooms: Vec<u64> = st
        .rooms
        .iter()
        .filter(|(_, r)| {
            r.kind == 1
                && (r.guards.is_some() || r.standby.as_ref().is_some_and(|s| s.at.is_some()))
        })
        .map(|(&id, _)| id)
        .collect();
    for rid in rooms {
        upkeep_room(st, rid);
    }
}

/// Le relais de secours prêt d'un monde : sa connexion et son pseudo.
fn ready_standby(st: &State, rid: u64) -> Option<(u64, String)> {
    let sb = st.rooms.get(&rid)?.standby.as_ref()?;
    sb.at.as_ref()?;
    let p = st.conns.get(&sb.host)?.pseudo.clone()?;
    Some((sb.host, p))
}

/// Témoins possibles pour un monde : des connexions de garant (de n'importe quel monde),
/// pas celle du relais de secours ni un gardien déjà nommé ; d'abord celles d'autres
/// machines (que celle du relais de secours, des autres témoins et de ce relais), puis
/// les plus aptes.
fn witness_candidates(st: &State, rid: u64, taken: &[String], keep: &[u64]) -> Vec<(u64, String)> {
    let standby_host = st
        .rooms
        .get(&rid)
        .and_then(|r| r.standby.as_ref())
        .map(|s| s.host);
    let cooling = cooling(st, rid);
    let apart = super::machines_apart(st) && !st.standby_local;
    let others: Vec<&Conn> = standby_host
        .iter()
        .chain(keep.iter())
        .filter_map(|id| st.conns.get(id))
        .collect();
    let mut out: Vec<(bool, f64, u64, String)> = st
        .conns
        .iter()
        .filter(|(&id, c)| {
            c.garant_only
                && !c.garant_banned
                && Some(id) != standby_host
                && c.pseudo.as_deref().is_some_and(|p| {
                    !taken.iter().any(|t| t.eq_ignore_ascii_case(p))
                        && !cooling.contains(&p.to_ascii_lowercase())
                })
        })
        .map(|(&id, c)| {
            let diverse = !apart || (!c.local && !others.iter().any(|o| same_machine(c, o)));
            let score = super::zones::aptitude(&super::zones::Candidate {
                id: 0,
                caps: c.caps,
                uptime: c.joined.elapsed(),
                ratio: c.pseudo.as_deref().map_or(0.0, |p| st.stats.ratio(p)),
                rtt: super::link_ms(c),
            });
            (diverse, score, id, c.pseudo.clone().unwrap_or_default())
        })
        .collect();
    out.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.total_cmp(&a.1)));
    out.into_iter().map(|(_, _, id, p)| (id, p)).collect()
}

fn upkeep_room(st: &mut State, rid: u64) {
    let now = now_ms(st);
    let standby = ready_standby(st, rid);
    // Relais de secours qui ne donne plus de bail (parti, coupé de ce relais) : il est
    // rendu, un autre sera choisi (`upkeep_standby`). Il reste gardien de la liste en cours
    // jusqu'au changement. (Pas pendant une pause : c'est peut-être lui qui reprendra.)
    if let (Some((host, p)), Some(g)) =
        (&standby, st.rooms.get(&rid).and_then(|r| r.guards.as_ref()))
    {
        if !g.warden.released()
            && g.warden.serving(now)
            && g.warden.current.is_standby(p)
            && g.warden.idle(p, now)
        {
            log::info!(
                "monde {rid} : le relais de secours ne répond plus aux battements, remplacé"
            );
            drop_standby(st, rid, *host);
            return;
        }
    }
    let Some(room) = st.rooms.get(&rid) else {
        return;
    };
    // Premiers gardiens : dès que le relais de secours est prêt.
    if room.guards.is_none() {
        let Some((host, p)) = standby else {
            return;
        };
        let candidates = witness_candidates(st, rid, std::slice::from_ref(&p), &[]);
        let target = guard::witness_target(candidates.len());
        let picks: Vec<(u64, String)> = candidates.into_iter().take(target).collect();
        let witnesses: Vec<String> = picks.iter().map(|(_, p)| p.clone()).collect();
        let warden = Warden::new(&st.cert_key, rid, Some(p.clone()), witnesses, now);
        let mut conns: HashMap<String, u64> = picks
            .iter()
            .map(|(id, p)| (p.to_ascii_lowercase(), *id))
            .collect();
        conns.insert(p.to_ascii_lowercase(), host);
        log::info!(
            "monde {rid} : gardiens (version 1) : relais de secours {p}, témoins {:?}",
            warden.current.witnesses
        );
        for (id, _) in &picks {
            send_witness_task(st, rid, *id);
        }
        let room = st.rooms.get_mut(&rid).expect("partie");
        room.guards = Some(RoomGuards {
            warden,
            conns,
            cooldown: HashMap::new(),
            paused_since: None,
            serving: true,
            last_beat: 0,
            last_heartbeat: 0,
            last_scan: now,
        });
    } else {
        reconfigure(st, rid, now, standby);
    }
    beat(st, rid, now);
    refresh_pause(st, rid, now);
    release_if_stuck(st, rid, now);
}

/// Pause sans issue : depuis `RELEASE_MS`, ce relais n'entend plus assez de témoins (ceux
/// qui ont voté ne lui redonneront jamais de bail). S'il entend encore le relais de
/// secours, il lui confie le monde (arrêt signé) plutôt que de le laisser en pause.
fn release_if_stuck(st: &mut State, rid: u64, now: u64) {
    let Some(g) = st.rooms.get_mut(&rid).and_then(|r| r.guards.as_mut()) else {
        return;
    };
    if g.warden.released() || g.warden.serving(now) {
        g.paused_since = None;
        return;
    }
    let since = *g.paused_since.get_or_insert(now);
    if now.saturating_sub(since) < guard::RELEASE_MS {
        return;
    }
    let Some(s) = g.warden.releasable() else {
        return;
    };
    if !g.warden.fresh(&s, now) {
        return;
    }
    let Some(conn) = g.conns.get(&s.to_ascii_lowercase()).copied() else {
        return;
    };
    g.warden.release();
    log::warn!("monde {rid} : en pause sans issue, confié à son relais de secours {s}");
    if let Some(c) = st.conns.get(&conn) {
        transport::push(
            &c.tx,
            &Response::Released {
                room: rid,
                sig: guard::sign_release(&st.cert_key, rid),
            },
        );
    }
    refresh_pause(st, rid, now);
}

/// Le relais de secours est rendu : il quitte le monde (un autre sera choisi, voir
/// `upkeep_standby`), et les joueurs l'apprennent. Il ne sera plus gardien de ce monde.
fn drop_standby(st: &mut State, rid: u64, host: u64) {
    let now = now_ms(st);
    if let Some(p) = st.conns.get(&host).and_then(|c| c.pseudo.clone()) {
        if let Some(g) = st.rooms.get_mut(&rid).and_then(|r| r.guards.as_mut()) {
            g.cooldown.insert(p.to_ascii_lowercase(), now + COOLDOWN_MS);
        }
    }
    super::leave_room(st, host);
    if let Some(c) = st.conns.get(&host) {
        transport::push(&c.tx, &Response::RoomClosed);
    }
    let Some(room) = st.rooms.get_mut(&rid) else {
        return;
    };
    if room.standby.as_ref().is_some_and(|s| s.host == host) {
        let server = room.server.clone();
        room.standby = None;
        room.build.set_replicating(false);
        st.bags.watch(&server, false);
        super::announce_standby(st, rid);
    }
}

/// Changement de gardiens : un seul à la fois (voir `Warden::reconfigure`). D'abord le
/// relais de secours, puis un témoin parti ou muet (remplacé, sinon retiré), puis un
/// témoin de plus jusqu'à ce que permettent les garants (`guard::witness_target`). Jamais
/// pendant une pause : le changement ne pourrait pas finir.
fn reconfigure(st: &mut State, rid: u64, now: u64, standby: Option<(u64, String)>) {
    let Some(g) = st.rooms.get(&rid).and_then(|r| r.guards.as_ref()) else {
        return;
    };
    if g.warden.changing() || g.warden.released() || !g.warden.serving(now) {
        return;
    }
    let cur = g.warden.current.clone();
    let want_standby = standby.as_ref().map(|(_, p)| p.clone());
    let same_standby = cur.standby.as_ref().map(|s| s.to_ascii_lowercase())
        == want_standby.as_ref().map(|s| s.to_ascii_lowercase());
    let mut witnesses = cur.witnesses.clone();
    let alive = |w: &String| {
        g.conns
            .get(&w.to_ascii_lowercase())
            .is_some_and(|id| st.conns.contains_key(id))
            && !g.warden.idle(w, now)
    };
    let dead: Vec<String> = witnesses.iter().filter(|w| !alive(w)).cloned().collect();
    // Rien à changer (ou seulement un témoin de plus à chercher, pas trop souvent : il
    // faut passer toutes les connexions en revue).
    let scan_due = now.saturating_sub(g.last_scan) >= SCAN_MS;
    if same_standby && dead.is_empty() && (witnesses.len() >= guard::WITNESSES_MAX || !scan_due) {
        return;
    }
    let keep: Vec<u64> = witnesses
        .iter()
        .filter(|w| alive(w))
        .filter_map(|w| g.conns.get(&w.to_ascii_lowercase()).copied())
        .collect();
    let mut taken: Vec<String> = witnesses.clone();
    taken.extend(want_standby.iter().cloned());
    taken.extend(cur.standby.iter().cloned());
    let candidates = witness_candidates(st, rid, &taken, &keep);
    if let Some(g) = st.rooms.get_mut(&rid).and_then(|r| r.guards.as_mut()) {
        g.last_scan = now;
    }
    let mut added: Option<(u64, String)> = None;
    if !same_standby {
        // Le relais de secours change (ou n'est plus là) : seul changement de ce tour.
        // (Un témoin devenu relais de secours quitte la liste des témoins dans le même
        // changement : c'est le même gardien.)
        if let Some(p) = &want_standby {
            witnesses.retain(|w| !w.eq_ignore_ascii_case(p));
        }
    } else if let Some(d) = dead.first() {
        let i = witnesses.iter().position(|w| w == d).expect("témoin");
        match candidates.first() {
            Some(c) => {
                witnesses[i] = c.1.clone();
                added = Some(c.clone());
            }
            None => {
                witnesses.remove(i);
            }
        }
    } else {
        let target = guard::witness_target(witnesses.len() + candidates.len());
        if witnesses.len() < target {
            if let Some(c) = candidates.first() {
                witnesses.push(c.1.clone());
                added = Some(c.clone());
            }
        } else {
            return;
        }
    }
    let root = st.cert_key.clone();
    let room = st.rooms.get_mut(&rid).expect("partie");
    let g = room.guards.as_mut().expect("gardiens");
    if !g
        .warden
        .reconfigure(&root, want_standby.clone(), witnesses, now)
    {
        return;
    }
    if let Some((host, p)) = &standby {
        g.conns.insert(p.to_ascii_lowercase(), *host);
    }
    if let Some((id, p)) = &added {
        g.conns.insert(p.to_ascii_lowercase(), *id);
    }
    if same_standby {
        if let Some(d) = dead.first() {
            g.cooldown.insert(d.to_ascii_lowercase(), now + COOLDOWN_MS);
        }
    }
    log::info!(
        "monde {rid} : gardiens (version {}) : relais de secours {:?}, témoins {:?}",
        g.warden.current.version,
        g.warden.current.standby,
        g.warden.current.witnesses
    );
    if let Some((id, _)) = added {
        send_witness_task(st, rid, id);
    }
}

/// Au garant choisi comme témoin : la clé des listes et le certificat de la clé de son
/// compte (pour entrer chez le relais de secours).
fn send_witness_task(st: &State, rid: u64, id: u64) {
    let Some(c) = st.conns.get(&id) else {
        return;
    };
    let (Some(pseudo), Some(key)) = (c.pseudo.clone(), super::account_key(st, id)) else {
        return;
    };
    let cert = minelib_net::sovereign::Cert::issue(
        &st.cert_key,
        rid,
        &pseudo,
        key,
        now_secs() + 24 * 3600,
    );
    transport::push(
        &c.tx,
        &Response::WitnessTask {
            room: rid,
            root: st.cert_key.verifying_key().to_bytes(),
            cert,
        },
    );
}

/// Battements aux gardiens (ceux de la liste en cours et des listes encore comptées).
fn beat(st: &mut State, rid: u64, now: u64) {
    let Some(room) = st.rooms.get(&rid) else {
        return;
    };
    let Some(g) = room.guards.as_ref() else {
        return;
    };
    if g.warden.released() || now.saturating_sub(g.last_beat) < guard::BEAT_MS {
        return;
    }
    let at = room.standby.as_ref().and_then(|s| s.at.clone());
    let msg = Response::Beat {
        t: now,
        committee: g.warden.current.clone(),
        standby: at,
        paused: !g.warden.serving(now),
    };
    for p in g.warden.targets() {
        if cut(st, &p) {
            continue;
        }
        if let Some(c) = g.conns.get(&p).and_then(|id| st.conns.get(id)) {
            transport::push(&c.tx, &msg);
        }
    }
    let room = st.rooms.get_mut(&rid).expect("partie");
    room.guards.as_mut().expect("gardiens").last_beat = now;
}

/// Les joueurs du monde savent s'il est en pause : tout de suite quand ça change, et deux
/// fois par seconde (un jeu qui n'entend plus rien cherche le relais de secours).
pub(crate) fn refresh_pause(st: &mut State, rid: u64, now: u64) {
    let Some(room) = st.rooms.get_mut(&rid) else {
        return;
    };
    let Some(g) = room.guards.as_mut() else {
        return;
    };
    let serving = g.warden.serving(now);
    let changed = serving != g.serving;
    if !changed && now.saturating_sub(g.last_heartbeat) < guard::HEARTBEAT_MS {
        return;
    }
    g.serving = serving;
    g.last_heartbeat = now;
    if changed {
        if serving {
            log::info!("monde {rid} : assez de gardiens entendus, le monde repart");
        } else {
            log::warn!(
                "monde {rid} : pas assez de gardiens entendus (version {}), monde en pause",
                g.warden.current.version
            );
        }
    }
    let to: Vec<u64> = room.everyone().collect();
    let msg = Response::Paused { paused: !serving };
    for id in to {
        if let Some(c) = st.conns.get(&id) {
            transport::push(&c.tx, &msg);
        }
    }
}

/// Bail d'un gardien.
pub(crate) fn lease(st: &mut State, rid: u64, id: u64, pseudo: &str, version: u64, t: u64) {
    if cut(st, pseudo) {
        return;
    }
    let now = now_ms(st);
    let Some(g) = st.rooms.get_mut(&rid).and_then(|r| r.guards.as_mut()) else {
        return;
    };
    if g.conns.get(&pseudo.to_ascii_lowercase()) != Some(&id) {
        return;
    }
    g.warden.lease(pseudo, version, t, now);
    refresh_pause(st, rid, now);
}

/// Un gardien s'en va proprement.
pub(crate) fn resign(st: &mut State, rid: u64, id: u64, pseudo: &str) {
    let now = now_ms(st);
    let Some(g) = st.rooms.get_mut(&rid).and_then(|r| r.guards.as_mut()) else {
        return;
    };
    if g.conns.get(&pseudo.to_ascii_lowercase()) != Some(&id) {
        return;
    }
    log::info!("monde {rid} : le gardien {pseudo} s'en va");
    g.warden.resign(pseudo);
    refresh_pause(st, rid, now);
}

/// Arrêt propre de ce relais : chaque monde gardé se met en pause pour toujours, et son
/// relais de secours le reprend aussitôt. Renvoie le nombre de mondes confiés.
pub(crate) fn release_all(st: &mut State) -> usize {
    let rooms: Vec<u64> = st
        .rooms
        .iter()
        .filter(|(_, r)| r.kind == 1)
        .map(|(&id, _)| id)
        .collect();
    let mut handed = 0;
    for rid in rooms {
        // (Relais de secours prêt depuis moins de 50 ms : ses premiers gardiens.)
        if st.rooms[&rid].guards.is_none() && ready_standby(st, rid).is_some() {
            upkeep_room(st, rid);
        }
        let now = now_ms(st);
        let Some(g) = st.rooms.get_mut(&rid).and_then(|r| r.guards.as_mut()) else {
            continue;
        };
        let to = g.warden.release();
        let conn = to
            .as_ref()
            .and_then(|p| g.conns.get(&p.to_ascii_lowercase()).copied());
        if let Some(c) = conn.and_then(|id| st.conns.get(&id)) {
            transport::push(
                &c.tx,
                &Response::Released {
                    room: rid,
                    sig: guard::sign_release(&st.cert_key, rid),
                },
            );
            handed += 1;
            log::info!("monde {rid} : arrêt propre, confié à son relais de secours");
        }
        refresh_pause(st, rid, now);
    }
    handed
}

// ---------------------------------------------------------------------------------------
// Côté relais de secours.

/// Relais de secours : ses baux (`Keeper` ; il ne vote pas, sa voix ne compterait pas), les
/// votes signés des témoins (la liste du vote, leur certificat, le vote ; par pseudo en
/// minuscules), et si le relais du monde se disait en pause à son dernier battement.
pub(crate) struct StandbyGuard {
    pub keeper: Keeper,
    pub votes: HashMap<String, (guard::Committee, minelib_net::sovereign::Cert, guard::Vote)>,
    pub root_paused: bool,
}

/// Battement du relais du monde reçu par le relais de secours : version à rappeler dans
/// son bail (aucune : il a voté).
pub(crate) fn standby_beat(st: &mut State, c: &guard::Committee, paused: bool) -> Option<u64> {
    let now = guard::clock();
    let g = st.standby.as_mut()?.guard.as_mut()?;
    let v = g.keeper.beat(c, now);
    if v.is_some() {
        g.root_paused = paused;
    }
    v
}

/// Relais de secours, régulièrement : reprise dès qu'un quorum de témoins d'une même liste
/// (qui le nomme) a voté. Le quorum de votes garantit que le relais du monde n'a plus
/// celui des baux, et ne le retrouvera jamais pour cette liste.
pub(crate) fn standby_tick(st: &mut State) -> bool {
    let Some(sb) = st.standby.as_mut() else {
        return false;
    };
    let Some(g) = sb.guard.as_mut() else {
        return false;
    };
    if sb.active {
        return false;
    }
    // (Un relais de secours honnête ne reprend pas un monde que le relais du monde sert
    // sous ses yeux : il faut qu'il ne l'entende plus, ou que celui-ci se dise en pause.
    // La sûreté n'en dépend pas, c'est le quorum de votes qui la donne ; mais un témoin
    // menteur seul ne suffit plus alors.)
    let silent = g
        .keeper
        .silent_for(guard::clock())
        .is_none_or(|t| t >= guard::VOTE_MS);
    if !silent && !g.root_paused {
        return false;
    }
    let votes = &g.votes;
    let me = g.keeper.me.clone();
    let lists: Vec<guard::Committee> = votes.values().map(|(c, _, _)| c.clone()).collect();
    let found = lists.into_iter().find(|c| {
        guard::enough_votes(c, &me, |w| {
            votes
                .get(w)
                .filter(|(vc, _, _)| vc.version == c.version)
                .map(|(_, _, v)| v.version)
        })
    });
    if let Some(committee) = found {
        // La preuve montrée aux joueurs : la liste, et les votes signés pour elle.
        let proof_votes = committee
            .witnesses
            .iter()
            .filter_map(|w| votes.get(&w.to_ascii_lowercase()))
            .filter(|(c, _, v)| c.version == committee.version && v.version == committee.version)
            .map(|(_, cert, v)| (cert.clone(), v.clone()))
            .collect();
        sb.proof = Some(guard::Proof::Votes {
            committee,
            votes: proof_votes,
        });
        sb.active = true;
        log::warn!(
            "relais de secours : un quorum de témoins n'entend plus le relais du monde, \
             reprise du monde « {} »",
            sb.info.name
        );
        return true;
    }
    false
}

/// Témoin entré chez le relais de secours (son certificat) : sa liste et son vote, gardé
/// s'il est bien signé de la clé de son compte.
pub(crate) fn witness_beat(
    st: &mut State,
    id: u64,
    cert: &minelib_net::sovereign::Cert,
    committee: &guard::Committee,
    vote: Option<&(guard::Committee, guard::Vote)>,
) {
    let now = guard::clock();
    let Some(sb) = st.standby.as_mut() else {
        return;
    };
    let room = sb.room;
    let root = sb.root;
    let Some(g) = sb.guard.as_mut() else {
        return;
    };
    g.keeper.learn(committee, now);
    // (Un vote ne compte que pour une liste signée par le relais du monde, qui nomme ce
    // témoin, et signé de la clé de son certificat.)
    if let Some((c, v)) = vote.filter(|(c, v)| {
        c.room == room
            && v.version == c.version
            && c.is_witness(&cert.pseudo)
            && c.valid(&root)
            && v.valid(&cert.key, room, &cert.pseudo)
    }) {
        g.votes.insert(
            cert.pseudo.to_ascii_lowercase(),
            (c.clone(), cert.clone(), v.clone()),
        );
    }
    // Le témoin est en retard d'une liste : il apprend la nôtre.
    if let Some(mine) = g
        .keeper
        .committee
        .as_ref()
        .filter(|m| m.version > committee.version)
    {
        if let Some(c) = st.conns.get(&id) {
            transport::push(
                &c.tx,
                &Response::Committee {
                    committee: mine.clone(),
                },
            );
        }
    }
}

/// Entrée d'un témoin chez le relais de secours (certificat, puis signature d'un défi).
pub(crate) fn witness_login(st: &mut State, session: &mut Session, req: Request) -> Response {
    use ed25519_dalek::Verifier;
    match req {
        Request::WitnessLogin { cert } => {
            let Some(sb) = st.standby.as_ref() else {
                return err(ErrorCode::Refused, "Pas de relais de secours ici.");
            };
            if !cert.valid(&sb.root, sb.room) || cert.expires < now_secs() {
                return err(ErrorCode::WrongCredentials, "certificat");
            }
            let mut challenge = [0u8; 32];
            rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut challenge);
            *session = Session::Witnessing { cert, challenge };
            Response::TakeoverChallenge {
                challenge,
                proof: None,
            }
        }
        Request::WitnessProve { signature } => {
            let Session::Witnessing { cert, challenge } = &*session else {
                return err(ErrorCode::NotLoggedIn, "");
            };
            let ok = ed25519_dalek::VerifyingKey::from_bytes(&cert.key)
                .ok()
                .zip(ed25519_dalek::Signature::from_slice(&signature).ok())
                .is_some_and(|(k, s)| k.verify(challenge, &s).is_ok());
            if !ok {
                *session = Session::Anonymous;
                return err(ErrorCode::WrongCredentials, "signature");
            }
            log::info!("relais de secours : témoin « {} » entré", cert.pseudo);
            *session = Session::Witness { cert: cert.clone() };
            Response::Done
        }
        _ => err(ErrorCode::Protocol, "témoin"),
    }
}
