//! (v232) Solo → Cartes FPS : les cartes FPS de ce PC et du relais, pour jouer contre des bots
//! sans passer par le multijoueur. Une carte du relais (ou téléchargée pour une partie en
//! ligne) est copiée dans ses mondes solo (« Télécharger »), une seule fois ; « Jouer contre
//! des bots » l'ouvre en mode FPS (un monde à soi garde son mode pour la suite).

use minelib_platform::{Input, KeyCode};
use minelib_render::Ui;

use crate::menu::{fit, Ctx, GOLD, GREY, WHITE};

/// Une ligne de la liste, déjà décidée par le jeu.
#[derive(Clone, Debug, PartialEq)]
pub struct SoloRow {
    pub name: String,
    pub detail: String,
    /// Joueurs d'une partie (format « 5 contre 5 »).
    pub players: u8,
    pub can_play: bool,
    pub can_download: bool,
}

/// État de l'écran gardé entre les images.
#[derive(Default)]
pub struct SoloMaps {
    pub selected: Option<usize>,
    pub scroll: usize,
    pub drag: bool,
    pub search: String,
    pub search_focus: bool,
    /// Dernier clic sur une ligne (index, horloge) : un double clic joue.
    pub click: Option<(usize, f32)>,
    /// Dernière demande de la liste du relais (horloge de l'écran).
    pub polled: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SoloMapsAction {
    None,
    Back,
    /// Redemander la liste du relais (toutes les 3 s).
    Poll,
    Refresh,
    Play(usize),
    Download(usize),
}

/// Ce que dit une ligne sous le nom de la carte.
pub fn detail(in_saves: bool, downloaded: bool, stored: bool, author: &str, configured: bool, plays: u32) -> String {
    let mut d = if in_saves {
        "dans tes mondes".to_string()
    } else if downloaded {
        "téléchargée".to_string()
    } else if !stored {
        "en cours d'envoi au relais…".to_string()
    } else {
        format!("à télécharger (de {author})")
    };
    if !configured {
        d += " - départs non réglés";
    }
    if plays > 0 {
        d += &format!(" - {plays} parties en ligne");
    }
    d
}

/// L'écran : liste des cartes (recherche par nom), jouer, télécharger.
#[allow(clippy::too_many_arguments)]
pub fn screen(
    c: &mut Ctx,
    input: &Input,
    st: &mut SoloMaps,
    rows: &[SoloRow],
    status: Option<&str>,
    logged_in: bool,
    busy: bool,
    time: f32,
) -> SoloMapsAction {
    let s = c.s;
    let ds = (s / 2.0).floor().max(1.0);
    let mut out = SoloMapsAction::None;
    if logged_in && time - st.polled >= 3.0 {
        st.polled = time;
        out = SoloMapsAction::Poll;
    }
    c.text_center("Cartes FPS", 10.0 * s, s * 2.0, WHITE);
    let hint = if logged_in {
        "Contre des bots. Celles du relais se téléchargent dans tes mondes."
    } else {
        "Contre des bots. Connecte-toi pour voir aussi les cartes du relais."
    };
    let w = (c.w / s - 24.0).clamp(260.0, 420.0) * s;
    let x = (c.w - w) / 2.0;
    c.text_center(&fit(hint, w, ds), 30.0 * s, ds, GREY);
    if c.text_field(
        "",
        &st.search,
        "rechercher une carte",
        c.w / 2.0,
        40.0 * s,
        w / s,
        st.search_focus,
        time,
    ) {
        st.search_focus = true;
    } else if c.click {
        st.search_focus = false;
    }
    if st.search_focus {
        crate::menu::edit_text(&mut st.search, input, 24);
    }
    let query = st.search.trim().to_lowercase();
    let shown: Vec<usize> = (0..rows.len())
        .filter(|&i| query.is_empty() || rows[i].name.to_lowercase().contains(&query))
        .collect();
    if st.selected.is_some_and(|i| !shown.contains(&i)) {
        st.selected = None;
    }
    let list_y = 66.0 * s;
    let by = c.h - 48.0 * s;
    let list_h = (by - 22.0 * s - list_y).max(30.0 * s);
    c.frame(x, list_y, w, list_h);
    let row_h = 26.0 * s;
    let vis = ((list_h - 4.0 * s) / row_h).floor().max(1.0) as usize;
    let over = c.hovered(x, list_y, w, list_h);
    Ctx::wheel(input, over, shown.len(), vis, &mut st.scroll);
    let bar = c.bar_w();
    c.scrollbar(
        x + w - bar - 3.0 * s,
        list_y + 2.0 * s,
        vis as f32 * row_h - s,
        shown.len(),
        vis,
        &mut st.scroll,
        &mut st.drag,
    );
    let right = if shown.len() > vis { bar + 4.0 * s } else { 0.0 };
    if shown.is_empty() {
        let t = if rows.is_empty() {
            "Aucune carte FPS : crée un monde, règle ses départs avec /fps set, ou télécharge-en une."
        } else {
            "Aucune carte ne correspond à la recherche."
        };
        c.text_center(&fit(t, w - 8.0 * s, ds), list_y + 14.0 * s, ds, GREY);
    }
    let big = s * 1.5;
    let mut double = None;
    for (k, &i) in shown.iter().enumerate().skip(st.scroll).take(vis) {
        let r = &rows[i];
        let y = list_y + 2.0 * s + (k - st.scroll) as f32 * row_h;
        let n = r.players.max(2) / 2;
        let fmt = format!("{n} contre {n}");
        let fw = Ui::text_width(&fmt, big);
        if c.list_row(
            x + 2.0 * s,
            y,
            w - 4.0 * s - right,
            row_h - s,
            &fit(&r.name, w - fw - 24.0 * s - right, s),
            &fit(&r.detail, w - fw - 24.0 * s - right, ds),
            st.selected == Some(i),
        ) {
            st.selected = Some(i);
            if st.click.is_some_and(|(j, t)| j == i && time - t < 0.4) {
                double = Some(i);
                st.click = None;
            } else {
                st.click = Some((i, time));
            }
        }
        c.ui.text(x + w - fw - 8.0 * s - right, y + 7.0 * s, big, WHITE, fmt);
    }
    if let Some(t) = status {
        c.text_center(&fit(t, w, s), by - 18.0 * s, s, GOLD);
    }
    let sel = st.selected.and_then(|i| rows.get(i));
    let cx = c.w / 2.0;
    let play = c.button(
        "Jouer contre des bots",
        cx - 95.0 * s,
        by,
        180.0,
        sel.is_some_and(|r| r.can_play) && !busy,
    );
    let download = c.button(
        "Télécharger",
        cx + 95.0 * s,
        by,
        180.0,
        sel.is_some_and(|r| r.can_download) && !busy,
    );
    let refresh = c.button("Actualiser", cx - 95.0 * s, by + 24.0 * s, 180.0, true);
    let back = c.button("Retour", cx + 95.0 * s, by + 24.0 * s, 180.0, true);
    if let Some(i) = double.filter(|&i| rows[i].can_play && !busy) {
        out = SoloMapsAction::Play(i);
    } else if play {
        if let Some(i) = st.selected {
            out = SoloMapsAction::Play(i);
        }
    } else if download {
        if let Some(i) = st.selected {
            out = SoloMapsAction::Download(i);
        }
    } else if refresh {
        out = SoloMapsAction::Refresh;
    }
    if back || (input.pressed(KeyCode::Escape) && !st.search_focus) {
        out = SoloMapsAction::Back;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_map_row_says_where_the_map_is() {
        assert_eq!(detail(true, false, true, "Bob", true, 0), "dans tes mondes");
        assert_eq!(detail(false, true, true, "Bob", true, 3), "téléchargée - 3 parties en ligne");
        assert_eq!(detail(false, false, true, "Bob", true, 0), "à télécharger (de Bob)");
        assert_eq!(
            detail(false, false, false, "Bob", false, 0),
            "en cours d'envoi au relais… - départs non réglés"
        );
    }
}
