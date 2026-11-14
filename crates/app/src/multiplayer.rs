//! Écrans du multijoueur : choix du mode (FPS ou construction).
//!
//! - FPS : pas de liste de serveurs. En haut le classement (50 meilleurs du monde, ou ses
//!   amis), en bas le choix de la carte (chaque carte une seule fois, par sa signature) ;
//!   « Rechercher » rejoint une partie en attente sur cette carte, ou en ouvre une.
//! - Construction : liste des mondes ouverts par les joueurs.

use minelib_render::Ui;

use crate::menu::{Ctx, GOLD, GREY, WHITE};

/// Mode de jeu en ligne.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Fps,
    Build,
}

impl Kind {
    pub fn title(self) -> &'static str {
        match self {
            Kind::Fps => "FPS",
            Kind::Build => "Construction",
        }
    }
}

/// Partie annoncée par le relais.
#[derive(Clone, Debug)]
pub struct ServerInfo {
    pub name: String,
    pub map: String,
    pub players: u32,
    pub max_players: u32,
    /// Latence mesurée vers le relais de la partie (ms).
    pub ping: u32,
    pub region: String,
}

/// Ce que l'écran demande au jeu.
pub enum Action {
    None,
    Back,
    Open(Kind),
    Refresh,
    /// Rejoindre la partie n° (indice dans la liste).
    Join(usize),
    /// Créer une partie (choix de la carte).
    Create,
}

/// AK stylisée, en cases d'une grille de 46 × 16 (x vers la droite, y vers le bas) : crosse,
/// carcasse, poignée, chargeur courbé, garde-main, tube de gaz, canon et guidon.
const AK: &[[f32; 4]] = &[
    // Crosse (bas en pente vers la carcasse).
    [0.0, 3.0, 3.0, 9.5],
    [3.0, 3.0, 6.0, 8.6],
    [6.0, 3.0, 8.0, 7.8],
    [8.0, 3.0, 11.0, 7.2],
    // Carcasse et hausse.
    [11.0, 3.0, 24.0, 7.2],
    [13.0, 2.0, 16.0, 3.0],
    // Poignée pistolet, penchée vers l'arrière.
    [16.0, 7.2, 19.5, 9.0],
    [15.4, 9.0, 18.9, 10.8],
    [14.8, 10.8, 18.3, 12.4],
    // Pontet.
    [19.5, 7.2, 21.0, 7.9],
    [20.4, 7.9, 21.0, 9.2],
    // Chargeur courbé vers l'avant.
    [21.0, 7.2, 25.0, 9.2],
    [21.6, 9.2, 25.8, 11.0],
    [22.4, 11.0, 26.8, 12.8],
    [23.4, 12.8, 27.8, 14.6],
    [24.6, 14.6, 28.6, 15.8],
    // Tube de gaz, garde-main, canon.
    [22.0, 2.0, 35.0, 3.0],
    [24.0, 3.0, 34.0, 6.4],
    [34.0, 4.0, 44.0, 5.2],
    // Bloc de gaz et guidon.
    [35.0, 2.0, 37.0, 5.2],
    [39.0, 0.4, 40.2, 4.0],
    [38.4, 3.0, 40.8, 5.6],
    // Bouche du canon.
    [44.0, 3.8, 46.0, 5.4],
];

/// Dessine l'AK en blanc, centrée en (`cx`, `cy`), large de `w` pixels.
fn draw_ak(ui: &mut Ui, cx: f32, cy: f32, w: f32, color: [f32; 4]) {
    let k = w / 46.0;
    let (x0, y0) = (cx - 23.0 * k, cy - 8.0 * k);
    let shadow = [0.0, 0.0, 0.0, 0.35];
    for pass in 0..2 {
        let (dx, c) = if pass == 0 {
            (k * 0.6, shadow)
        } else {
            (0.0, color)
        };
        for r in AK {
            ui.rect(
                x0 + r[0] * k + dx,
                y0 + r[1] * k + dx,
                (r[2] - r[0]) * k,
                (r[3] - r[1]) * k,
                c,
            );
        }
    }
}

/// Grande carte carrée cliquable avec une illustration et un titre. Renvoie vrai si cliquée.
fn card(
    c: &mut Ctx,
    x: f32,
    y: f32,
    size: f32,
    title: &str,
    art: &dyn Fn(&mut Ui, f32, f32, f32),
) -> bool {
    let s = c.s;
    let hover = c.hovered(x, y, size, size);
    // Cadre sombre du pack, liseré doré au survol.
    if hover {
        let t = 2.0 * s;
        c.ui.rect(x - t, y - t, size + 2.0 * t, t, GOLD);
        c.ui.rect(x - t, y + size, size + 2.0 * t, t, GOLD);
        c.ui.rect(x - t, y, t, size, GOLD);
        c.ui.rect(x + size, y, t, size, GOLD);
    }
    c.frame(x, y, size, size);
    art(c.ui, x + size / 2.0, y + size * 0.44, size);
    // Titre aussi grand que possible sans dépasser la carte.
    let ts = (s * 2.0).min(
        ((size * 0.86) / Ui::text_width(title, 1.0))
            .floor()
            .max(1.0),
    );
    let tw = Ui::text_width(title, ts);
    c.ui.text(
        x + (size - tw) / 2.0,
        y + size * 0.8,
        ts,
        if hover { GOLD } else { WHITE },
        title,
    );
    hover && c.click
}

/// Choix du mode : FPS à gauche (AK blanche), construction à droite (bloc blanc en 3D).
pub fn mode_screen(c: &mut Ctx) -> Action {
    c.background();
    let s = c.s;
    c.text_center("Multijoueur", c.h * 0.1, s * 2.0, WHITE);
    let size = (c.h * 0.5).min(c.w * 0.36);
    let gap = size * 0.12;
    let y = (c.h - size) / 2.0;
    let left = c.w / 2.0 - gap / 2.0 - size;
    let right = c.w / 2.0 + gap / 2.0;
    let fps = card(c, left, y, size, Kind::Fps.title(), &|ui, cx, cy, size| {
        draw_ak(ui, cx, cy, size * 0.78, WHITE);
    });
    let build = card(
        c,
        right,
        y,
        size,
        Kind::Build.title(),
        &|ui, cx, cy, size| {
            ui.model(crate::hand::white_cube_icon(cx, cy, size * 0.42));
        },
    );
    let back = c.button("Retour", c.w / 2.0, y + size + 16.0 * s, 200.0, true);
    if fps {
        Action::Open(Kind::Fps)
    } else if build {
        Action::Open(Kind::Build)
    } else if back {
        Action::Back
    } else {
        Action::None
    }
}

/// Liste des parties d'un mode. `selected` : ligne choisie.
pub fn list_screen(
    c: &mut Ctx,
    kind: Kind,
    servers: &[ServerInfo],
    selected: &mut Option<usize>,
) -> Action {
    c.background();
    let s = c.s;
    let title = match kind {
        Kind::Fps => "Parties FPS en attente de joueurs",
        Kind::Build => "Serveurs de construction",
    };
    c.text_center(title, 12.0 * s, s * 2.0, WHITE);
    let list_w = (c.w - 40.0 * s).min(360.0 * s);
    let list_x = (c.w - list_w) / 2.0;
    let list_y = 40.0 * s;
    let list_h = c.h - list_y - 64.0 * s;
    c.frame(list_x, list_y, list_w, list_h);
    // En-têtes des colonnes.
    let ds = (s / 2.0).floor().max(1.0);
    let cols = [
        ("Nom", 0.03),
        ("Carte", 0.45),
        ("Joueurs", 0.68),
        ("Région", 0.8),
        ("Ping", 0.92),
    ];
    for (label, f) in cols {
        c.ui.text(list_x + list_w * f, list_y + 6.0 * s, ds, GREY, label);
    }
    let row_h = 24.0 * s;
    let top = list_y + 16.0 * s;
    if servers.is_empty() {
        let msg = match kind {
            Kind::Fps => "Aucune partie en attente pour le moment.",
            Kind::Build => "Aucun serveur en ligne pour le moment.",
        };
        let tw = Ui::text_width(msg, s);
        c.ui.text(
            list_x + (list_w - tw) / 2.0,
            list_y + list_h / 2.0 - 10.0 * s,
            s,
            GREY,
            msg,
        );
        let note = match kind {
            Kind::Fps => "Crée la tienne avec « Créer une partie ».",
            Kind::Build => "Ouvre un de tes mondes avec « Ouvrir un monde ».",
        };
        let tw = Ui::text_width(note, ds);
        c.ui.text(
            list_x + (list_w - tw) / 2.0,
            list_y + list_h / 2.0 + 4.0 * s,
            ds,
            GREY,
            note,
        );
    }
    let visible = ((list_h - 18.0 * s) / row_h).floor().max(0.0) as usize;
    for (i, sv) in servers.iter().take(visible).enumerate() {
        let y = top + i as f32 * row_h;
        let detail = format!("{} - {}", sv.map, sv.region);
        if c.list_row(
            list_x + 2.0 * s,
            y,
            list_w - 4.0 * s,
            row_h - s,
            &sv.name,
            &detail,
            *selected == Some(i),
        ) {
            *selected = Some(i);
        }
        let info = if sv.max_players == 0 {
            format!("{}", sv.players)
        } else {
            format!("{}/{}", sv.players, sv.max_players)
        };
        c.ui.text(list_x + list_w * 0.68, y + 4.0 * s, s, WHITE, info);
        c.ui.text(
            list_x + list_w * 0.92,
            y + 4.0 * s,
            s,
            WHITE,
            format!("{}", sv.ping),
        );
    }
    if selected.is_some_and(|i| i >= servers.len()) {
        *selected = None;
    }
    // Boutons : rejoindre, créer, actualiser, retour.
    let by = c.h - 48.0 * s;
    let cx = c.w / 2.0;
    let bw = 150.0;
    let half = (bw / 2.0 + 3.0) * s;
    let join = c.button("Rejoindre", cx - half, by, bw, selected.is_some());
    let label = if kind == Kind::Build {
        "Ouvrir un monde"
    } else {
        "Créer une partie"
    };
    let create = c.button(label, cx + half, by, bw, true);
    let refresh = c.button("Actualiser", cx - half, by + 24.0 * s, bw, true);
    let back = c.button("Retour", cx + half, by + 24.0 * s, bw, true);
    if back {
        Action::Back
    } else if let (true, Some(i)) = (join, *selected) {
        Action::Join(i)
    } else if create {
        Action::Create
    } else if refresh {
        Action::Refresh
    } else {
        Action::None
    }
}

/// Une carte proposée (réunion des cartes de ce PC et de celles du relais, une seule
/// fois par signature).
#[derive(Clone, Debug)]
pub struct MapRow {
    pub sig: [u8; 32],
    pub name: String,
    pub author: String,
    /// Dossier sur ce PC, si on l'a.
    pub local: Option<std::path::PathBuf>,
    pub waiting: u32,
    pub plays: u32,
    /// Départs et sites définis (`/fps set`).
    pub configured: bool,
}

pub enum FpsAction {
    None,
    Back,
    Refresh,
    /// Onglet du classement : amis (vrai) ou monde.
    Tab(bool),
    Search(usize),
    AddFriend(String),
}

/// État de l'écran FPS gardé entre les images.
#[derive(Default)]
pub struct FpsHome {
    pub friends_tab: bool,
    pub scroll: usize,
    pub map_scroll: usize,
    pub selected: Option<usize>,
    pub friend_field: String,
    pub friend_focus: bool,
}

/// Écran FPS : classement au-dessus, choix de la carte en dessous.
#[allow(clippy::too_many_arguments)]
pub fn fps_screen(
    c: &mut Ctx,
    input: &minelib_platform::Input,
    st: &mut FpsHome,
    board: Option<&(
        bool,
        Vec<minelib_net::proto::RankEntry>,
        Option<minelib_net::proto::RankEntry>,
    )>,
    maps: &[MapRow],
    status: Option<&str>,
    busy: bool,
    time: f32,
) -> FpsAction {
    c.background();
    let s = c.s;
    let ds = (s / 2.0).floor().max(1.0);
    c.text_center("FPS - compétitif 5 contre 5", 8.0 * s, s * 1.5, WHITE);
    let w = (c.w - 24.0 * s).min(440.0 * s);
    let x = (c.w - w) / 2.0;
    // Onglets du classement.
    let tab_y = 24.0 * s;
    let tab_w = 130.0;
    let world = c.button(
        "Top 50 mondial",
        x + tab_w * s / 2.0,
        tab_y,
        tab_w,
        st.friends_tab,
    );
    let friends = c.button(
        "Amis",
        x + tab_w * s * 1.5 + 4.0 * s,
        tab_y,
        tab_w,
        !st.friends_tab,
    );
    let mut action = FpsAction::None;
    if world {
        st.friends_tab = false;
        st.scroll = 0;
        action = FpsAction::Tab(false);
    } else if friends {
        st.friends_tab = true;
        st.scroll = 0;
        action = FpsAction::Tab(true);
    }
    // Ajout d'un ami (onglet amis).
    if st.friends_tab {
        let fx = x + w - 90.0 * s;
        if c.text_field(
            "",
            &st.friend_field,
            "pseudo d'un ami",
            fx,
            tab_y,
            100.0,
            st.friend_focus,
            time,
        ) {
            st.friend_focus = true;
        }
        if st.friend_focus {
            crate::menu::edit_text(&mut st.friend_field, input, 16);
        }
        let add = c.button(
            "Ajouter",
            x + w - 18.0 * s,
            tab_y,
            34.0,
            !st.friend_field.is_empty(),
        );
        if add || (st.friend_focus && input.pressed(minelib_platform::KeyCode::Enter)) {
            action = FpsAction::AddFriend(std::mem::take(&mut st.friend_field));
        }
    }
    // Classement.
    let list_y = tab_y + 24.0 * s;
    let list_h = (c.h * 0.42).max(80.0 * s);
    c.frame(x, list_y, w, list_h);
    let cols: [(&str, f32); 9] = [
        ("#", 0.02),
        ("Joueur", 0.08),
        ("Elo", 0.36),
        ("Matchs", 0.45),
        ("Vict.", 0.55),
        ("K/D", 0.64),
        ("HS %", 0.73),
        ("ADR", 0.82),
        ("MVP", 0.91),
    ];
    for (label, f) in cols {
        c.ui.text(x + w * f, list_y + 5.0 * s, ds, GREY, label);
    }
    let row = 11.0 * s;
    let top = list_y + 15.0 * s;
    let visible = ((list_h - 30.0 * s) / row).floor().max(1.0) as usize;
    let line = |c: &mut Ctx, y: f32, e: &minelib_net::proto::RankEntry, color: [f32; 4]| {
        let kd = e.kills as f32 / e.deaths.max(1) as f32;
        let hs = e.headshots as f32 * 100.0 / e.kills.max(1) as f32;
        let adr = e.damage as f32 / e.rounds.max(1) as f32;
        let win = e.wins as f32 * 100.0 / e.matches.max(1) as f32;
        let vals = [
            format!("{}", e.rank),
            e.pseudo.clone(),
            format!("{}", e.elo),
            format!("{}", e.matches),
            format!("{win:.0} %"),
            format!("{kd:.2}"),
            format!("{hs:.0}"),
            format!("{adr:.0}"),
            format!("{}", e.mvps),
        ];
        for ((_, f), v) in cols.iter().zip(vals) {
            c.ui.text(x + w * f, y, s, color, v);
        }
    };
    match board.filter(|b| b.0 == st.friends_tab) {
        None => {
            let t = "Chargement du classement…";
            c.ui.text(
                x + (w - Ui::text_width(t, s)) / 2.0,
                top + 20.0 * s,
                s,
                GREY,
                t,
            );
        }
        Some((_, list, me)) => {
            if list.is_empty() {
                let t = if st.friends_tab {
                    "Ajoute des amis pour comparer vos statistiques."
                } else {
                    "Personne n'a encore fini de partie classée."
                };
                c.ui.text(
                    x + (w - Ui::text_width(t, s)) / 2.0,
                    top + 20.0 * s,
                    s,
                    GREY,
                    t,
                );
            }
            let over = c.hovered(x, list_y, w, list_h);
            if over && input.scroll.abs() >= 1.0 && list.len() > visible {
                let d = -input.scroll.signum() as i32;
                st.scroll = (st.scroll as i32 + d).clamp(0, (list.len() - visible) as i32) as usize;
            }
            let my = me.as_ref().map(|m| m.pseudo.clone());
            for (i, e) in list.iter().skip(st.scroll).take(visible).enumerate() {
                let col = if Some(&e.pseudo) == my.as_ref() {
                    GOLD
                } else {
                    WHITE
                };
                line(c, top + i as f32 * row, e, col);
            }
            // Sa propre ligne, en bas.
            if let Some(m) = me {
                let y = list_y + list_h - 13.0 * s;
                c.ui.rect(
                    x + 2.0 * s,
                    y - 2.0 * s,
                    w - 4.0 * s,
                    row + s,
                    [1.0, 1.0, 1.0, 0.06],
                );
                line(c, y, m, GOLD);
            }
        }
    }
    // Choix de la carte.
    let my = list_y + list_h + 6.0 * s;
    c.ui.text(x, my, s, GREY, "Carte");
    let maps_y = my + 10.0 * s;
    let maps_h = (c.h - maps_y - 56.0 * s).max(40.0 * s);
    c.frame(x, maps_y, w, maps_h);
    let row_h = 24.0 * s;
    let vis = ((maps_h - 4.0 * s) / row_h).floor().max(1.0) as usize;
    if c.hovered(x, maps_y, w, maps_h) && input.scroll.abs() >= 1.0 && maps.len() > vis {
        let d = -input.scroll.signum() as i32;
        st.map_scroll = (st.map_scroll as i32 + d).clamp(0, (maps.len() - vis) as i32) as usize;
    }
    if maps.is_empty() {
        let t = "Aucune carte : crée un monde, règle les départs avec /fps set, et reviens.";
        c.ui.text(
            x + (w - Ui::text_width(t, ds)) / 2.0,
            maps_y + 14.0 * s,
            ds,
            GREY,
            t,
        );
    }
    for (k, m) in maps.iter().enumerate().skip(st.map_scroll).take(vis) {
        let y = maps_y + 2.0 * s + (k - st.map_scroll) as f32 * row_h;
        let mut detail = if m.local.is_some() {
            "sur ce PC".to_string()
        } else {
            format!("à télécharger (de {})", m.author)
        };
        if !m.configured {
            detail += " - départs non réglés";
        }
        if m.plays > 0 {
            detail += &format!(" - {} parties", m.plays);
        }
        detail += &format!(" - {}", minelib_net::crypto::hex(&m.sig[..4]));
        if c.list_row(
            x + 2.0 * s,
            y,
            w - 4.0 * s,
            row_h - s,
            &m.name,
            &detail,
            st.selected == Some(k),
        ) {
            st.selected = Some(k);
        }
        if m.waiting > 0 {
            let t = format!("{} en attente", m.waiting);
            c.ui.text(
                x + w - Ui::text_width(&t, s) - 8.0 * s,
                y + 4.0 * s,
                s,
                GOLD,
                t,
            );
        }
    }
    if st.selected.is_some_and(|i| i >= maps.len()) {
        st.selected = None;
    }
    if let Some(t) = status {
        c.text_center(t, c.h - 60.0 * s, s, GOLD);
    }
    let by = c.h - 48.0 * s;
    let cx = c.w / 2.0;
    let half = (180.0 / 2.0 + 3.0) * s;
    let search = c.button(
        "Rechercher une partie",
        cx - half,
        by,
        180.0,
        st.selected.is_some() && !busy,
    );
    let refresh = c.button("Actualiser", cx + half, by, 180.0, true);
    let back = c.button("Retour", cx, by + 24.0 * s, 150.0, true);
    if back || input.pressed(minelib_platform::KeyCode::Escape) {
        return FpsAction::Back;
    }
    if let (true, Some(i)) = (search, st.selected) {
        return FpsAction::Search(i);
    }
    if refresh {
        return FpsAction::Refresh;
    }
    action
}
