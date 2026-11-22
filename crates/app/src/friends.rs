//! (v231) Amis, partout : un écran ouvert depuis l'accueil comme depuis le menu du jeu
//! (Échap). On y voit ses amis, en ligne d'abord, et où ils sont (au menu, dans quel monde de
//! construction, en partie FPS) ; on les rejoint dans leur monde, on les invite dans le sien,
//! on en ajoute, on en retire. On est amis dans les deux sens : ajouter quelqu'un lui envoie
//! une demande (Accepter, ou x pour refuser), et on ne sait rien de lui tant qu'il ne l'a pas
//! acceptée. Les invitations et les demandes reçues y attendent (avec une notification à leur
//! arrivée), et un bouton rend invisible : hors ligne pour ses amis, sans invitations.

use minelib_net::proto::{self, FriendPlace};
use minelib_platform::{Input, KeyCode};
use minelib_render::Ui;

use crate::menu::{fit, Ctx, GOLD, GREY, WHITE};

/// Invitation reçue dans le monde de construction d'un ami.
#[derive(Clone, Debug, PartialEq)]
pub struct WorldInvite {
    pub from: String,
    pub room: u64,
    pub name: String,
}

/// Ce que l'écran garde d'une image à l'autre.
#[derive(Default)]
pub struct FriendsUi {
    pub search: String,
    pub focus: bool,
    pub scroll: usize,
    pub drag: bool,
    /// Dernière demande de la liste (elle est redemandée toutes les `REFRESH_S` s tant que
    /// l'écran est ouvert).
    pub asked: Option<std::time::Instant>,
}

/// La liste est redemandée au relais toutes les… (s) tant que l'écran est ouvert.
pub const REFRESH_S: f32 = 5.0;

/// Ce que l'écran montre.
pub struct FriendsView<'a> {
    pub friends: &'a [(String, bool)],
    pub places: &'a [FriendPlace],
    pub invites: &'a [WorldInvite],
    /// Demandes d'ami reçues.
    pub requests: &'a [String],
    pub invisible: bool,
    /// Monde de construction en ligne où l'on est (sa salle du relais) : on peut y inviter
    /// ses amis.
    pub in_world: Option<u64>,
    /// Message à montrer sous le titre (invitation envoyée, erreur…).
    pub note: Option<&'a str>,
}

/// Ce que le joueur a demandé.
#[derive(Clone, Debug, PartialEq)]
pub enum FriendsAction {
    None,
    Back,
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

/// L'écran des amis (au-dessus du jeu, ou sur le fond des menus).
pub fn screen(
    c: &mut Ctx,
    input: &Input,
    st: &mut FriendsUi,
    v: &FriendsView,
    time: f32,
) -> FriendsAction {
    let s = c.s;
    let ds = (s / 2.0).floor().max(1.0);
    let mut out = FriendsAction::None;
    c.text_center("Amis", 10.0 * s, s * 2.0, WHITE);
    let w = (c.w / s - 24.0).clamp(220.0, 360.0) * s;
    let x = (c.w - w) / 2.0;
    let lx = x + 6.0 * s;
    let inner = w - 12.0 * s;
    if let Some(n) = v.note {
        c.text_center(&fit(n, w, ds), 30.0 * s, ds, GOLD);
    }
    // Ajouter un ami (son pseudo), et son statut à soi.
    let fy = 50.0 * s;
    let field_w = (inner / s - 128.0).max(60.0);
    if c.text_field(
        "Ajouter un ami",
        &st.search,
        "pseudo",
        lx + field_w * s / 2.0,
        fy,
        field_w,
        st.focus,
        time,
    ) {
        st.focus = true;
    } else if c.click && !c.hovered(lx, fy, field_w * s, 20.0 * s) {
        st.focus = false;
    }
    if st.focus {
        crate::menu::edit_text(&mut st.search, input, 16);
    }
    let add = c.button(
        "+",
        lx + field_w * s + 14.0 * s,
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
    let status = if v.invisible {
        "Statut : invisible"
    } else {
        "Statut : en ligne"
    };
    if c.button(status, x + w - 6.0 * s - 46.0 * s, fy, 92.0, true) {
        out = FriendsAction::Invisible(!v.invisible);
    }
    let mut y = fy + 26.0 * s;
    // Invitations reçues.
    for (i, inv) in v.invites.iter().enumerate().take(3) {
        c.ui.rect(lx, y, inner, 22.0 * s, [0.9, 0.75, 0.2, 0.15]);
        // Le pseudo en grand, le monde en plus petit (en entier si possible).
        let who = fit(&inv.from, inner - 150.0 * s, s);
        let ww = Ui::text_width(&who, s);
        c.ui.text(lx + 3.0 * s, y + 7.0 * s, s, GOLD, who);
        let rest = inner - 150.0 * s - ww - 4.0 * s;
        if rest > 20.0 * s {
            let t = fit(&format!("t'invite dans « {} »", inv.name), rest, ds);
            c.ui.text(lx + 7.0 * s + ww, y + 9.0 * s, ds, GOLD, t);
        }
        if c.button("Rejoindre", lx + inner - 102.0 * s, y + s, 70.0, true) {
            out = FriendsAction::Join(inv.room);
        }
        if c.button("x", lx + inner - 14.0 * s, y + s, 20.0, true) {
            out = FriendsAction::Dismiss(i);
        }
        y += 24.0 * s;
    }
    // Demandes d'ami reçues.
    for who in v.requests.iter().take(3) {
        c.ui.rect(lx, y, inner, 22.0 * s, [0.3, 0.6, 0.9, 0.15]);
        let name = fit(who, inner - 150.0 * s, s);
        let ww = Ui::text_width(&name, s);
        c.ui.text(lx + 3.0 * s, y + 7.0 * s, s, WHITE, name);
        let rest = inner - 150.0 * s - ww - 4.0 * s;
        if rest > 20.0 * s {
            let t = fit("veut être ton ami", rest, ds);
            c.ui.text(lx + 7.0 * s + ww, y + 9.0 * s, ds, GREY, t);
        }
        if c.button("Accepter", lx + inner - 102.0 * s, y + s, 70.0, true) {
            out = FriendsAction::Add(who.clone());
        }
        if c.button("x", lx + inner - 14.0 * s, y + s, 20.0, true) {
            out = FriendsAction::Decline(who.clone());
        }
        y += 24.0 * s;
    }
    // Les amis : en ligne d'abord.
    let back_y = c.h - 26.0 * s;
    let list_h = (back_y - y - 6.0 * s).max(16.0 * s);
    c.frame(x, y, w, list_h);
    let row = 16.0 * s;
    let mut order: Vec<usize> = (0..v.friends.len()).collect();
    order.sort_by_key(|&i| (!v.friends[i].1, v.friends[i].0.to_lowercase()));
    let vis = ((list_h - 4.0 * s) / row).floor().max(1.0) as usize;
    let over = c.hovered(x, y, w, list_h);
    Ctx::wheel(input, over, order.len(), vis, &mut st.scroll);
    let bar = c.bar_w();
    c.scrollbar(
        x + w - bar - 2.0 * s,
        y + 2.0 * s,
        vis as f32 * row,
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
        c.ui.text(
            lx,
            y + 6.0 * s,
            ds,
            GREY,
            fit("Ajoute un ami avec son pseudo (ou /ami <pseudo> en jeu).", inner, ds),
        );
    }
    for (k, &i) in order.iter().skip(st.scroll).take(vis).enumerate() {
        let (name, online) = &v.friends[i];
        let place = v.places.get(i);
        let ry = y + 2.0 * s + k as f32 * row;
        let dot = if *online {
            [0.3, 0.85, 0.35, 1.0]
        } else {
            [0.45, 0.45, 0.45, 1.0]
        };
        c.ui.rect(lx + 2.0 * s, ry + 6.0 * s, 4.0 * s, 4.0 * s, dot);
        // Boutons à droite : retirer, inviter (si l'on est dans un monde), rejoindre.
        let mut bx = lx + inner_f;
        bx -= 22.0 * s;
        if c.button("-", bx + 11.0 * s, ry - 2.0 * s, 20.0, true) {
            out = FriendsAction::Remove(name.clone());
        }
        let joinable = *online && place.is_some_and(|p| p.joinable && p.room != 0);
        let here = v.in_world.is_some_and(|w| place.is_some_and(|p| p.room == w));
        if joinable && !here {
            bx -= 64.0 * s;
            if c.button("Rejoindre", bx + 31.0 * s, ry - 2.0 * s, 62.0, true) {
                out = FriendsAction::Join(place.map_or(0, |p| p.room));
            }
        }
        if v.in_world.is_some() && *online && !here {
            bx -= 54.0 * s;
            if c.button("Inviter", bx + 26.0 * s, ry - 2.0 * s, 52.0, true) {
                out = FriendsAction::Invite(name.clone());
            }
        }
        let text_w = bx - lx - 10.0 * s;
        let what = place_text(*online, place);
        let nw = Ui::text_width(name, s).min(text_w);
        c.ui.text(lx + 9.0 * s, ry + 4.0 * s, s, WHITE, fit(name, text_w, s));
        let rest = text_w - nw - 6.0 * s;
        if rest > 20.0 * s {
            c.ui.text(
                lx + 9.0 * s + nw + 6.0 * s,
                ry + 5.0 * s,
                ds,
                GREY,
                fit(&what, rest, ds),
            );
        }
    }
    if c.button("Retour", c.w / 2.0, back_y, 160.0, true)
        || (input.pressed(KeyCode::Escape) && !st.focus)
    {
        out = FriendsAction::Back;
    }
    out
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
}
