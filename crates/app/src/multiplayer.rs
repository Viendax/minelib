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
    /// Joueurs d'une partie (10 : 5 contre 5, la seule classée).
    pub players: u8,
    /// Le relais l'a entière (sinon : en cours d'envoi, pas encore téléchargeable).
    pub stored: bool,
}

pub enum FpsAction {
    None,
    Back,
    /// Mise à jour automatique de la liste des cartes (toutes les 3 s).
    Poll,
    Refresh,
    /// Onglet du classement : amis (vrai) ou monde.
    Tab(bool),
    Search(usize),
    AddFriend(String),
    /// Lobby : action (`proto::PARTY_*`) et pseudo visé.
    Party(u8, String),
    /// Couleur préférée (255 : aucune).
    Color(u8),
}

/// Ce que le panneau du lobby affiche.
pub struct LobbyView<'a> {
    pub me: &'a str,
    /// Amis et s'ils sont en ligne.
    pub friends: &'a [(String, bool)],
    /// Membres du lobby (l'hôte d'abord ; vide : seul).
    pub members: &'a [String],
    /// Invitations reçues (pseudos).
    pub invites: &'a [String],
    /// Couleur préférée (255 : aucune).
    pub color: u8,
}

/// État de l'écran FPS gardé entre les images.
#[derive(Default)]
pub struct FpsHome {
    /// Onglet du classement : mondial (sinon les amis, par défaut).
    pub world_tab: bool,
    /// Recherche d'amis (panneau du lobby).
    pub search: String,
    pub search_focus: bool,
    /// Menu du clic droit sur un membre du lobby (pseudo, position).
    pub member_menu: Option<(String, f32, f32)>,
    pub friends_scroll: usize,
    pub scroll: usize,
    pub map_scroll: usize,
    pub selected: Option<usize>,
    /// Dernière mise à jour automatique de la liste des cartes (horloge de l'écran).
    pub polled: f32,
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
    lobby: LobbyView,
) -> FpsAction {
    c.background();
    let s = c.s;
    let ds = (s / 2.0).floor().max(1.0);
    // Panneau du lobby à droite, le reste à gauche.
    let panel_w = (c.w * 0.26).clamp(120.0 * s, 170.0 * s);
    let avail = c.w - panel_w - 12.0 * s;
    let panel_action = lobby_panel(c, input, st, &lobby, avail + 6.0 * s, panel_w, time);
    let title = "FPS - compétitif 5 contre 5";
    let tw = Ui::text_width(title, s * 1.5);
    c.ui.text((avail - tw) / 2.0, 8.0 * s, s * 1.5, WHITE, title);
    let w = (avail - 16.0 * s).min(440.0 * s);
    let x = (avail - w) / 2.0;
    // Onglets du classement : les amis d'abord, le top mondial à droite.
    let tab_y = 24.0 * s;
    let tab_w = 110.0;
    let friends = c.button("Amis", x + tab_w * s / 2.0, tab_y, tab_w, st.world_tab);
    let world = c.button(
        "Top mondial",
        x + tab_w * s * 1.5 + 4.0 * s,
        tab_y,
        tab_w,
        !st.world_tab,
    );
    let mut action = panel_action;
    if world {
        st.world_tab = true;
        st.scroll = 0;
        action = FpsAction::Tab(false);
    } else if friends {
        st.world_tab = false;
        st.scroll = 0;
        action = FpsAction::Tab(true);
    }
    // Classement.
    let list_y = tab_y + 24.0 * s;
    let list_h = (c.h * 0.28).max(70.0 * s);
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
    match board.filter(|b| b.0 != st.world_tab) {
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
                let t = if !st.world_tab {
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
    let row_h = 26.0 * s;
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
        if !m.stored && m.local.is_none() {
            detail = "en cours d'envoi au relais…".to_string();
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
        // Joueurs, bien visibles : « 3 / 10 » en attente, ou le format de la carte.
        let n = m.players.max(2) as u32;
        let (t, col) = if m.waiting > 0 {
            (format!("{} / {n}", m.waiting), GOLD)
        } else {
            (format!("{} contre {}", n / 2, n / 2), WHITE)
        };
        let big = s * 1.5;
        c.ui.text(
            x + w - Ui::text_width(&t, big) - 8.0 * s,
            y + 3.0 * s,
            big,
            col,
            t,
        );
        if n != 10 {
            let r = "hors classement";
            c.ui.text(
                x + w - Ui::text_width(r, ds) - 8.0 * s,
                y + 16.0 * s,
                ds,
                GREY,
                r,
            );
        }
    }
    if st.selected.is_some_and(|i| i >= maps.len()) {
        st.selected = None;
    }
    if let Some(t) = status {
        let t = crate::menu::fit(t, avail - 8.0 * s, s);
        let tw = Ui::text_width(&t, s);
        c.ui.text((avail - tw) / 2.0, c.h - 60.0 * s, s, GOLD, t);
    }
    let by = c.h - 48.0 * s;
    let cx = avail / 2.0;
    let half = (150.0 / 2.0 + 3.0) * s;
    // En lobby, seul son hôte lance (pour tout le monde).
    let leader = lobby.members.first().is_none_or(|m| m.eq_ignore_ascii_case(lobby.me));
    let label = if leader { "Jouer" } else { "L'hôte lance" };
    let search = c.button(
        label,
        cx - half,
        by,
        150.0,
        st.selected.is_some() && !busy && leader,
    );
    let refresh = c.button("Actualiser", cx + half, by, 150.0, true);
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
    if matches!(action, FpsAction::None) && (time - st.polled).abs() > 3.0 {
        st.polled = time;
        return FpsAction::Poll;
    }
    action
}

/// Couleurs des joueurs (lobby, bandeau des têtes, triangles) : vert, jaune, orange, bleu,
/// violet.
pub const PLAYER_COLORS: [[f32; 4]; 5] = [
    [0.30, 0.85, 0.35, 1.0],
    [0.95, 0.85, 0.20, 1.0],
    [0.98, 0.55, 0.15, 1.0],
    [0.30, 0.60, 1.00, 1.0],
    [0.70, 0.40, 0.95, 1.0],
];

/// Panneau du lobby, à droite : recherche d'amis en haut, amis en dessous (clic : inviter),
/// invitations reçues, et les membres du lobby en bas (clic droit : exclure, promouvoir).
fn lobby_panel(
    c: &mut Ctx,
    input: &minelib_platform::Input,
    st: &mut FpsHome,
    v: &LobbyView,
    x: f32,
    w: f32,
    time: f32,
) -> FpsAction {
    use crate::menu::fit;
    use minelib_net::proto;
    let s = c.s;
    let ds = (s / 2.0).floor().max(1.0);
    let mut out = FpsAction::None;
    let (y0, h) = (8.0 * s, c.h - 16.0 * s);
    c.frame(x, y0, w, h);
    let inner = w - 12.0 * s;
    let lx = x + 6.0 * s;
    // Recherche : filtre les amis ; « + » ajoute ce pseudo en ami.
    let field_w = (inner / s - 26.0).max(40.0);
    let fy = y0 + 16.0 * s;
    if c.text_field(
        "Amis",
        &st.search,
        "rechercher",
        lx + field_w * s / 2.0,
        fy,
        field_w,
        st.search_focus,
        time,
    ) {
        st.search_focus = true;
    } else if c.click && !c.hovered(x, y0, w, h) {
        st.search_focus = false;
    }
    if st.search_focus {
        crate::menu::edit_text(&mut st.search, input, 16);
    }
    let add = c.button(
        "+",
        x + w - 6.0 * s - 11.0 * s,
        fy,
        22.0,
        !st.search.trim().is_empty(),
    );
    if add || (st.search_focus && input.pressed(minelib_platform::KeyCode::Enter)) {
        let p = st.search.trim().to_string();
        if !p.is_empty() {
            out = FpsAction::AddFriend(p);
            st.search.clear();
        }
    }
    // Couleur préférée : celle de ses têtes et de son triangle (si personne ne l'a déjà
    // dans l'équipe).
    let cy = fy + 24.0 * s;
    c.ui.text(lx, cy + 2.0 * s, ds, GREY, "Ta couleur");
    let sq = 9.0 * s;
    let cx0 = lx + Ui::text_width("Ta couleur", ds) + 4.0 * s;
    for (k, col) in PLAYER_COLORS.iter().enumerate() {
        let bx = cx0 + k as f32 * (sq + 2.0 * s);
        if bx + sq > lx + inner {
            break;
        }
        let chosen = v.color == k as u8;
        if chosen {
            c.ui.rect(bx - s, cy - s, sq + 2.0 * s, sq + 2.0 * s, WHITE);
        }
        c.ui.rect(bx, cy, sq, sq, *col);
        if c.hovered(bx, cy, sq, sq) && c.click {
            out = FpsAction::Color(if chosen { 255 } else { k as u8 });
        }
    }
    let mut y = cy + 14.0 * s;
    // Invitations reçues.
    for from in v.invites.iter().take(2) {
        c.ui.rect(lx, y, inner, 34.0 * s, [0.9, 0.75, 0.2, 0.15]);
        let t = fit(&format!("{from} t'invite"), inner - 4.0 * s, s);
        c.ui.text(lx + 3.0 * s, y + 3.0 * s, s, GOLD, t);
        let bw = (inner / s - 6.0) / 2.0;
        let ok = c.button("Accepter", lx + bw * s / 2.0 + s, y + 13.0 * s, bw, true);
        let no = c.button(
            "Refuser",
            lx + inner - bw * s / 2.0 - s,
            y + 13.0 * s,
            bw,
            true,
        );
        if ok {
            out = FpsAction::Party(proto::PARTY_ACCEPT, from.clone());
        } else if no {
            out = FpsAction::Party(proto::PARTY_DECLINE, from.clone());
        }
        y += 38.0 * s;
    }
    // Membres du lobby, en bas : 5 places.
    let slot_h = 14.0 * s;
    let members_h = 12.0 * s + slot_h * proto::PARTY_MAX as f32 + 24.0 * s;
    let members_y = y0 + h - members_h;
    // Amis (en ligne d'abord), filtrés par la recherche.
    let q = st.search.trim().to_lowercase();
    let mut list: Vec<&(String, bool)> = v
        .friends
        .iter()
        .filter(|(p, _)| q.is_empty() || p.to_lowercase().contains(&q))
        .collect();
    list.sort_by_key(|(p, on)| (!on, p.to_lowercase()));
    let row = 12.0 * s;
    let list_h = (members_y - y - 6.0 * s).max(row);
    let vis = (list_h / row).floor().max(1.0) as usize;
    if c.hovered(x, y, w, list_h) && input.scroll.abs() >= 1.0 && list.len() > vis {
        let d = -input.scroll.signum() as i32;
        st.friends_scroll =
            (st.friends_scroll as i32 + d).clamp(0, (list.len() - vis) as i32) as usize;
    }
    st.friends_scroll = st.friends_scroll.min(list.len().saturating_sub(vis));
    if list.is_empty() {
        let t = if v.friends.is_empty() {
            "Ajoute un ami avec +"
        } else {
            "Aucun ami trouvé"
        };
        c.ui.text(lx, y + 2.0 * s, ds, GREY, fit(t, inner, ds));
    }
    let in_lobby = |p: &str| v.members.iter().any(|m| m.eq_ignore_ascii_case(p));
    for (i, (p, on)) in list.iter().skip(st.friends_scroll).take(vis).enumerate() {
        let ry = y + i as f32 * row;
        let hover = c.hovered(lx, ry, inner, row);
        let can = *on && !in_lobby(p) && v.members.len() < proto::PARTY_MAX;
        if hover && can {
            c.ui.rect(lx, ry, inner, row, [1.0, 1.0, 1.0, 0.08]);
        }
        let dot = if *on {
            [0.3, 0.85, 0.35, 1.0]
        } else {
            [0.45, 0.45, 0.45, 1.0]
        };
        c.ui.rect(lx + 2.0 * s, ry + 4.0 * s, 4.0 * s, 4.0 * s, dot);
        let hint = if hover && can { "inviter" } else { "" };
        let hw = Ui::text_width(hint, ds);
        let name = fit(p, inner - 12.0 * s - hw, s);
        c.ui.text(
            lx + 9.0 * s,
            ry + 2.0 * s,
            s,
            if *on { WHITE } else { GREY },
            name,
        );
        if !hint.is_empty() {
            c.ui
                .text(lx + inner - hw - 2.0 * s, ry + 4.0 * s, ds, GOLD, hint);
        }
        if hover && can && c.click {
            out = FpsAction::Party(proto::PARTY_INVITE, p.clone());
        }
    }
    // Lobby.
    c.ui.rect(lx, members_y - 3.0 * s, inner, s, [1.0, 1.0, 1.0, 0.15]);
    c.ui.text(lx, members_y, ds, GREY, "Lobby (clic droit : options)");
    let leader = v.members.first().map(String::as_str);
    let i_lead = leader.is_none_or(|l| l.eq_ignore_ascii_case(v.me));
    let shown: Vec<String> = if v.members.is_empty() {
        vec![v.me.to_string()]
    } else {
        v.members.to_vec()
    };
    let right = input.mouse_pressed(minelib_platform::Mouse::Right);
    for (k, col) in PLAYER_COLORS.iter().enumerate().take(proto::PARTY_MAX) {
        let sy = members_y + 10.0 * s + k as f32 * slot_h;
        c.ui.rect(lx, sy, inner, slot_h - s, [0.0, 0.0, 0.0, 0.25]);
        let Some(p) = shown.get(k) else {
            c.ui.text(lx + 9.0 * s, sy + 3.0 * s, ds, GREY, "place libre");
            continue;
        };
        c.ui
            .rect(lx + 2.0 * s, sy + 3.0 * s, 4.0 * s, 6.0 * s, *col);
        let tag = if k == 0 && v.members.len() > 1 {
            " (hôte)"
        } else {
            ""
        };
        let t = fit(&format!("{p}{tag}"), inner - 12.0 * s, s);
        let me = p.eq_ignore_ascii_case(v.me);
        c.ui.text(
            lx + 9.0 * s,
            sy + 2.0 * s,
            s,
            if me { GOLD } else { WHITE },
            t,
        );
        if right && c.hovered(lx, sy, inner, slot_h) && i_lead && !me {
            if let Some((mx, my)) = c.cursor {
                st.member_menu = Some((p.clone(), mx, my));
            }
        }
    }
    if v.members.len() > 1 {
        let ly = members_y + 12.0 * s + slot_h * proto::PARTY_MAX as f32;
        if c.button("Quitter le lobby", x + w / 2.0, ly, inner / s, true) {
            out = FpsAction::Party(proto::PARTY_LEAVE, String::new());
        }
    }
    // Menu du clic droit : exclure, promouvoir hôte.
    if let Some((p, mx, my)) = st.member_menu.clone() {
        let (mw, mh) = (100.0 * s, 56.0 * s);
        let mx = mx.min(c.w - mw - 2.0 * s);
        let my = my.min(c.h - mh - 2.0 * s);
        c.ui.rect(mx, my, mw, mh, [0.05, 0.05, 0.07, 0.97]);
        c.ui.text(mx + 4.0 * s, my + 2.0 * s, ds, GREY, fit(&p, mw - 8.0 * s, ds));
        let kick = c.button("Exclure", mx + mw / 2.0, my + 10.0 * s, 96.0, true);
        let promote = c.button("Promouvoir hôte", mx + mw / 2.0, my + 33.0 * s, 96.0, true);
        if kick {
            out = FpsAction::Party(proto::PARTY_KICK, p);
            st.member_menu = None;
        } else if promote {
            out = FpsAction::Party(proto::PARTY_PROMOTE, p);
            st.member_menu = None;
        } else if ((c.click && !c.hovered(mx, my, mw, mh)) || right)
            && (!right || !c.hovered(lx, members_y, inner, members_h))
        {
            st.member_menu = None;
        }
    }
    out
}
