//! (v231) Amis, partout. (v233) Un module, comme celui du lobby FPS (Multijoueur → FPS) : un
//! panneau à droite de l'accueil et du menu du jeu (Échap), sans bouton pour l'ouvrir. On y
//! voit ses amis, en ligne d'abord, et où ils sont (au menu, dans quel monde de construction,
//! en partie FPS) ; on les rejoint dans leur monde, on les invite dans le sien (clic sur la
//! ligne, ou clic droit : options), on en ajoute (« + »), on en retire. On est amis dans les
//! deux sens : ajouter quelqu'un lui envoie une demande (Accepter, ou x), et on ne sait rien
//! de lui tant qu'il ne l'a pas acceptée. Les invitations et les demandes reçues y attendent
//! (avec une notification à leur arrivée), et le statut rend invisible : hors ligne pour ses
//! amis, sans invitations.

use minelib_net::proto::{self, FriendPlace};
use minelib_platform::{Input, KeyCode, Mouse};
use minelib_render::Ui;

use crate::menu::{fit, Ctx, GOLD, GREY, WHITE};

/// Invitation reçue dans le monde de construction d'un ami.
#[derive(Clone, Debug, PartialEq)]
pub struct WorldInvite {
    pub from: String,
    pub room: u64,
    pub name: String,
}

/// Ce que le panneau garde d'une image à l'autre.
#[derive(Default)]
pub struct FriendsUi {
    /// Recherche dans ses amis (« + » : ajouter ce pseudo).
    pub search: String,
    pub focus: bool,
    pub scroll: usize,
    pub drag: bool,
    /// Dernière demande de la liste (redemandée toutes les `REFRESH_S` s tant que le
    /// panneau est affiché).
    pub asked: Option<std::time::Instant>,
    /// Menu du clic droit : l'ami (son pseudo) et où.
    pub menu: Option<(String, f32, f32)>,
}

/// La liste est redemandée au relais toutes les… (s) tant que le panneau est affiché.
pub const REFRESH_S: f32 = 5.0;

/// Ce que le panneau montre.
pub struct FriendsView<'a> {
    /// Compte connecté (sinon : une invitation à se connecter).
    pub logged_in: bool,
    pub friends: &'a [(String, bool)],
    pub places: &'a [FriendPlace],
    pub invites: &'a [WorldInvite],
    /// Demandes d'ami reçues.
    pub requests: &'a [String],
    pub invisible: bool,
    /// Monde de construction en ligne où l'on est (sa salle du relais) : on peut y inviter
    /// ses amis.
    pub in_world: Option<u64>,
    /// Message à montrer sous la recherche (invitation envoyée, erreur…).
    pub note: Option<&'a str>,
}

/// Ce que le joueur a demandé.
#[derive(Clone, Debug, PartialEq)]
pub enum FriendsAction {
    None,
    /// Pas connecté : aller se connecter.
    Login,
    Add(String),
    Remove(String),
    /// Rejoindre le monde de construction `room` (d'un ami, ou d'une invitation).
    Join(u64),
    Invite(String),
    /// Invisible (vrai) ou non.
    Invisible(bool),
    /// Oublier l'invitation `i`.
    Dismiss(usize),
    /// Refuser la demande d'ami de ce joueur (l'accepter : `Add`).
    Decline(String),
}

/// Où est un ami, en mots.
pub fn place_text(online: bool, p: Option<&FriendPlace>) -> String {
    match (online, p) {
        (_, Some(p)) if p.kind == proto::FRIEND_PENDING => "demande envoyée".into(),
        (false, _) => "hors ligne".into(),
        (true, Some(p)) if p.kind == proto::FRIEND_BUILD => format!("construit dans « {} »", p.name),
        (true, Some(p)) if p.kind == proto::FRIEND_FPS => format!("en partie FPS ({})", p.name),
        (true, _) => "au menu".into(),
    }
}

/// Largeur du panneau (comme celui du lobby FPS) pour cet écran.
pub fn panel_width(c: &Ctx) -> f32 {
    (c.w * 0.26).clamp(120.0 * c.s, 170.0 * c.s)
}

/// Ce qu'on peut faire avec l'ami `i` : le rejoindre (sa salle), l'inviter.
fn actions(v: &FriendsView, i: usize) -> (Option<u64>, bool) {
    let online = v.friends.get(i).is_some_and(|f| f.1);
    let place = v.places.get(i);
    let here = v.in_world.is_some_and(|w| place.is_some_and(|p| p.room == w));
    let join = place
        .filter(|p| online && p.joinable && p.room != 0 && !here)
        .map(|p| p.room);
    // Aussi un ami hors ligne : l'invitation l'attend (pas qui n'a pas accepté).
    let pending = place.is_some_and(|p| p.kind == proto::FRIEND_PENDING);
    let invite = v.in_world.is_some() && !pending && !here;
    (join, invite)
}

/// Le panneau des amis, dans ce rectangle.
#[allow(clippy::too_many_arguments)]
pub fn panel(
    c: &mut Ctx,
    input: &Input,
    st: &mut FriendsUi,
    v: &FriendsView,
    x: f32,
    y0: f32,
    w: f32,
    h: f32,
    time: f32,
) -> FriendsAction {
    let s = c.s;
    let ds = (s / 2.0).floor().max(1.0);
    let mut out = FriendsAction::None;
    c.frame(x, y0, w, h);
    let inner = w - 12.0 * s;
    let lx = x + 6.0 * s;
    if !v.logged_in {
        c.ui.text(lx, y0 + 6.0 * s, s, WHITE, "Amis");
        let t = "Connecte-toi pour retrouver tes amis, les rejoindre et les inviter.";
        let mut ty = y0 + 20.0 * s;
        for line in wrap(t, inner, ds) {
            c.ui.text(lx, ty, ds, GREY, line);
            ty += 6.0 * ds + 3.0 * s;
        }
        if c.button("Se connecter", x + w / 2.0, ty + 4.0 * s, inner / s, true) {
            out = FriendsAction::Login;
        }
        return out;
    }
    // Recherche : filtre ses amis ; « + » ajoute ce pseudo en ami.
    let field_w = (inner / s - 26.0).max(40.0);
    let fy = y0 + 16.0 * s;
    if c.text_field(
        "Amis",
        &st.search,
        "rechercher",
        lx + field_w * s / 2.0,
        fy,
        field_w,
        st.focus,
        time,
    ) {
        st.focus = true;
    } else if c.click && !c.hovered(x, y0, w, h) {
        st.focus = false;
    }
    if st.focus {
        crate::menu::edit_text(&mut st.search, input, 16);
    }
    let add = c.button(
        "+",
        x + w - 6.0 * s - 11.0 * s,
        fy,
        22.0,
        !st.search.trim().is_empty(),
    );
    if add || (st.focus && input.pressed(KeyCode::Enter)) {
        let p = st.search.trim().to_string();
        if !p.is_empty() {
            out = FriendsAction::Add(p);
            st.search.clear();
        }
    }
    // Statut : en ligne ou invisible.
    let sy = fy + 23.0 * s;
    let status = if v.invisible {
        "Statut : invisible"
    } else {
        "Statut : en ligne"
    };
    if c.button(status, x + w / 2.0, sy, inner / s, true) {
        out = FriendsAction::Invisible(!v.invisible);
    }
    let mut y = sy + 23.0 * s;
    if let Some(n) = v.note {
        for line in wrap(n, inner, ds).into_iter().take(2) {
            c.ui.text(lx, y, ds, GOLD, line);
            y += 6.0 * ds + 2.0 * s;
        }
        y += 2.0 * s;
    }
    // Un grand bouton et un « x » (refuser, ignorer), toujours lisibles.
    let bw = inner / s - 26.0;
    let xs = lx + inner - 11.0 * s;
    // Demandes d'ami reçues.
    for who in v.requests.iter().take(2) {
        c.ui.rect(lx, y, inner, 34.0 * s, [0.3, 0.6, 0.9, 0.15]);
        let t = fit(&format!("{who} veut être ton ami"), inner - 4.0 * s, ds);
        c.ui.text(lx + 3.0 * s, y + 4.0 * s, ds, WHITE, t);
        if c.button("Accepter", lx + bw * s / 2.0 + s, y + 13.0 * s, bw, true) {
            out = FriendsAction::Add(who.clone());
        }
        if c.button("x", xs, y + 13.0 * s, 20.0, true) {
            out = FriendsAction::Decline(who.clone());
        }
        y += 36.0 * s;
    }
    // Invitations reçues dans le monde d'un ami.
    for (i, inv) in v.invites.iter().enumerate().take(2) {
        c.ui.rect(lx, y, inner, 34.0 * s, [0.9, 0.75, 0.2, 0.15]);
        let t = fit(
            &format!("{} t'invite dans « {} »", inv.from, inv.name),
            inner - 4.0 * s,
            ds,
        );
        c.ui.text(lx + 3.0 * s, y + 4.0 * s, ds, GOLD, t);
        if c.button("Rejoindre", lx + bw * s / 2.0 + s, y + 13.0 * s, bw, true) {
            out = FriendsAction::Join(inv.room);
        }
        if c.button("x", xs, y + 13.0 * s, 20.0, true) {
            out = FriendsAction::Dismiss(i);
        }
        y += 36.0 * s;
    }
    // Les amis (en ligne d'abord), filtrés par la recherche : deux lignes, le pseudo et où il
    // est. Clic : le rejoindre, ou l'inviter (dans un monde) ; clic droit : options.
    let q = st.search.trim().to_lowercase();
    let mut order: Vec<usize> = (0..v.friends.len())
        .filter(|&i| q.is_empty() || v.friends[i].0.to_lowercase().contains(&q))
        .collect();
    order.sort_by_key(|&i| (!v.friends[i].1, v.friends[i].0.to_lowercase()));
    let foot = 10.0 * ds;
    let list_y = y;
    let list_h = (y0 + h - 4.0 * s - foot - list_y).max(16.0 * s);
    let row = 20.0 * s;
    let vis = (list_h / row).floor().max(1.0) as usize;
    let over = c.hovered(x, list_y, w, list_h);
    Ctx::wheel(input, over, order.len(), vis, &mut st.scroll);
    let bar = c.bar_w();
    c.scrollbar(
        lx + inner - bar,
        list_y,
        vis as f32 * row - s,
        order.len(),
        vis,
        &mut st.scroll,
        &mut st.drag,
    );
    let inner_f = if order.len() > vis {
        inner - bar - 2.0 * s
    } else {
        inner
    };
    if order.is_empty() {
        let t = if v.friends.is_empty() {
            "Ajoute un ami : son pseudo, puis +"
        } else {
            "Aucun ami trouvé"
        };
        c.ui.text(lx, list_y + 2.0 * s, ds, GREY, fit(t, inner, ds));
    }
    let right = input.mouse_pressed(Mouse::Right);
    for (k, &i) in order.iter().skip(st.scroll).take(vis).enumerate() {
        let (name, online) = &v.friends[i];
        let place = v.places.get(i);
        let ry = list_y + k as f32 * row;
        let (join, invite) = actions(v, i);
        // L'action du clic : inviter dans son monde, sinon le rejoindre.
        let hint = if invite {
            "inviter"
        } else if join.is_some() {
            "rejoindre"
        } else {
            ""
        };
        let hover = c.hovered(lx, ry, inner_f, row) && st.menu.is_none();
        if hover && !hint.is_empty() {
            c.ui.rect(lx, ry, inner_f, row, [1.0, 1.0, 1.0, 0.08]);
        }
        let dot = if *online {
            [0.3, 0.85, 0.35, 1.0]
        } else {
            [0.45, 0.45, 0.45, 1.0]
        };
        c.ui.rect(lx + 2.0 * s, ry + 4.0 * s, 4.0 * s, 4.0 * s, dot);
        let shown_hint = if hover { hint } else { "" };
        let hw = Ui::text_width(shown_hint, ds);
        c.ui.text(
            lx + 9.0 * s,
            ry + 2.0 * s,
            s,
            if *online { WHITE } else { GREY },
            fit(name, inner_f - 12.0 * s - hw, s),
        );
        c.ui.text(
            lx + 9.0 * s,
            ry + 11.0 * s,
            ds,
            GREY,
            fit(&place_text(*online, place), inner_f - 12.0 * s, ds),
        );
        if !shown_hint.is_empty() {
            c.ui.text(lx + inner_f - hw - 2.0 * s, ry + 4.0 * s, ds, GOLD, shown_hint);
        }
        if hover && c.click {
            if invite {
                out = FriendsAction::Invite(name.clone());
            } else if let Some(room) = join {
                out = FriendsAction::Join(room);
            }
        }
        if right && c.hovered(lx, ry, inner_f, row) {
            if let Some((mx, my)) = c.cursor {
                st.menu = Some((name.clone(), mx, my));
            }
        }
    }
    c.ui.text(lx, y0 + h - foot, ds, GREY, fit("clic droit : options", inner, ds));
    // Menu du clic droit : rejoindre, inviter, retirer.
    if let Some((who, mx, my)) = st.menu.clone() {
        let i = v.friends.iter().position(|f| f.0 == who);
        let (join, invite) = i.map_or((None, false), |i| actions(v, i));
        let (mw, mh) = (100.0 * s, 79.0 * s);
        let mx = mx.min(c.w - mw - 2.0 * s);
        let my = my.min(c.h - mh - 2.0 * s);
        c.ui.rect(mx, my, mw, mh, [0.05, 0.05, 0.07, 0.97]);
        c.ui.text(mx + 4.0 * s, my + 2.0 * s, ds, GREY, fit(&who, mw - 8.0 * s, ds));
        let j = c.button("Rejoindre", mx + mw / 2.0, my + 10.0 * s, 96.0, join.is_some());
        let inv = c.button("Inviter", mx + mw / 2.0, my + 33.0 * s, 96.0, invite);
        let rm = c.button("Retirer de mes amis", mx + mw / 2.0, my + 56.0 * s, 96.0, true);
        if j {
            out = join.map_or(FriendsAction::None, FriendsAction::Join);
            st.menu = None;
        } else if inv {
            out = FriendsAction::Invite(who);
            st.menu = None;
        } else if rm {
            out = FriendsAction::Remove(who);
            st.menu = None;
        } else if ((c.click || right) && !c.hovered(mx, my, mw, mh)) || i.is_none() {
            st.menu = None;
        }
    }
    out
}

/// Coupe un texte en lignes de cette largeur (par mots).
fn wrap(text: &str, w: f32, scale: f32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let next = if cur.is_empty() {
            word.to_string()
        } else {
            format!("{cur} {word}")
        };
        if Ui::text_width(&next, scale) > w && !cur.is_empty() {
            lines.push(std::mem::take(&mut cur));
            cur = word.to_string();
        } else {
            cur = next;
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_friend_is_shown_where_he_is() {
        let build = FriendPlace {
            kind: proto::FRIEND_BUILD,
            room: 7,
            name: "Château".into(),
            joinable: true,
        };
        assert_eq!(place_text(true, Some(&build)), "construit dans « Château »");
        assert_eq!(place_text(false, Some(&build)), "hors ligne");
        assert_eq!(place_text(true, Some(&FriendPlace::default())), "au menu");
        assert_eq!(place_text(true, None), "au menu");
        let fps = FriendPlace {
            kind: proto::FRIEND_FPS,
            room: 9,
            name: "dust".into(),
            joinable: false,
        };
        assert_eq!(place_text(true, Some(&fps)), "en partie FPS (dust)");
        // Il ne nous a pas encore ajouté : on ne sait rien de lui.
        let pending = FriendPlace {
            kind: proto::FRIEND_PENDING,
            ..Default::default()
        };
        assert_eq!(place_text(false, Some(&pending)), "demande envoyée");
    }

    /// (v233) Ce qu'un clic sur un ami propose : le rejoindre dans son monde (ouvert à nous,
    /// pas celui où l'on est), l'inviter dans le nôtre (même hors ligne : l'invitation
    /// l'attend), rien pour qui n'a pas accepté la demande.
    #[test]
    fn a_click_on_a_friend_joins_or_invites_him() {
        let friends = vec![
            ("Bob".to_string(), true),
            ("Cid".to_string(), false),
            ("Dan".to_string(), false),
        ];
        let places = vec![
            FriendPlace {
                kind: proto::FRIEND_BUILD,
                room: 7,
                name: "Château".into(),
                joinable: true,
            },
            FriendPlace::default(),
            FriendPlace {
                kind: proto::FRIEND_PENDING,
                ..Default::default()
            },
        ];
        let view = |in_world| FriendsView {
            logged_in: true,
            friends: &friends,
            places: &places,
            invites: &[],
            requests: &[],
            invisible: false,
            in_world,
            note: None,
        };
        // Au menu : on rejoint Bob, on n'invite personne.
        assert_eq!(actions(&view(None), 0), (Some(7), false));
        assert_eq!(actions(&view(None), 1), (None, false));
        // Dans son monde à soi : Bob se rejoint ou s'invite, Cid (hors ligne) s'invite, pas Dan.
        assert_eq!(actions(&view(Some(3)), 0), (Some(7), true));
        assert_eq!(actions(&view(Some(3)), 1), (None, true));
        assert_eq!(actions(&view(Some(3)), 2), (None, false));
        // Déjà dans le monde de Bob : rien.
        assert_eq!(actions(&view(Some(7)), 0), (None, false));
    }

    #[test]
    fn long_texts_wrap_by_words() {
        let w = Ui::text_width("un deux trois", 1.0);
        let lines = wrap("un deux trois quatre cinq", w, 1.0);
        assert_eq!(lines, vec!["un deux trois".to_string(), "quatre cinq".to_string()]);
    }
}
