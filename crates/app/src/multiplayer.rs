//! Écrans du multijoueur : choix du mode (FPS ou construction).
//!
//! - FPS : pas de liste de serveurs. En haut le classement (50 meilleurs du monde, ou ses
//!   amis), en bas le choix de la carte (chaque carte une seule fois, par sa signature) ;
//!   « Rechercher » rejoint une partie en attente sur cette carte, ou en ouvre une. (v340)
//!   En haut à gauche, le format (« Équipe : 5v5 ») : seules les cartes de ce format sont
//!   montrées, les certifiées d'abord, puis les plus jouées ; « Ajouter une carte » met un de
//!   ses mondes dans la liste de tous.
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
    /// (v311) Mondes de construction : record de joueurs présents en même temps, dernière
    /// activité (secondes depuis 1970 ; 0 : inconnue), monde endormi (on n'y entre pas) et
    /// qui en garde le dernier état.
    pub peak: u32,
    pub active: u64,
    pub asleep: bool,
    pub keeper: String,
    /// (v325) Endormi, ce joueur peut le réveiller (il l'a créé, ou il en garde le dernier
    /// état).
    pub can_wake: bool,
}

/// (v311) « à l'instant », « il y a 5 min », « il y a 3 h », « il y a 12 j ».
pub fn ago(now: u64, then: u64) -> String {
    let d = now.saturating_sub(then);
    match d {
        0..=89 => "à l'instant".to_string(),
        90..=5399 => format!("il y a {} min", (d + 30) / 60),
        5400..=129_599 => format!("il y a {} h", (d + 1800) / 3600),
        _ => format!("il y a {} j", (d + 43_200) / 86_400),
    }
}

/// (v311) Ce qui est écrit sous le nom d'une partie : sa carte et son relais ; pour un
/// monde de construction, son record de joueurs et sa dernière activité ; pour un monde
/// endormi, depuis quand et chez qui est son dernier état.
pub fn detail(sv: &ServerInfo, now: u64) -> String {
    if sv.asleep {
        let mut d = format!("{} - endormi", sv.map);
        if sv.active > 0 {
            d += &format!(", {}", ago(now, sv.active));
        }
        if sv.peak > 0 {
            d += &format!(" - record {}", sv.peak);
        }
        if !sv.keeper.is_empty() {
            d += &format!(" - dernier état chez {}", sv.keeper);
        }
        return d;
    }
    let mut d = format!("{} - {}", sv.map, sv.region);
    if sv.peak > sv.players {
        d += &format!(" - record {}", sv.peak);
    }
    if sv.active > 0 {
        d += &format!(" - actif {}", ago(now, sv.active));
    }
    d
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
/// Recherche au-dessus d'une liste de serveurs.
#[derive(Default)]
pub struct ListSearch {
    pub text: String,
    pub focus: bool,
}

#[allow(clippy::too_many_arguments)]
pub fn list_screen(
    c: &mut Ctx,
    input: &minelib_platform::Input,
    kind: Kind,
    servers: &[ServerInfo],
    selected: &mut Option<usize>,
    search: &mut ListSearch,
    time: f32,
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
    // Recherche (nom, carte, région), au-dessus de la liste.
    let field_w = list_w / s;
    if c.text_field(
        "",
        &search.text,
        "rechercher un serveur",
        list_x + list_w / 2.0,
        34.0 * s,
        field_w,
        search.focus,
        time,
    ) {
        search.focus = true;
    } else if c.click {
        search.focus = false;
    }
    if search.focus {
        crate::menu::edit_text(&mut search.text, input, 32);
    }
    let query = search.text.trim().to_lowercase();
    let shown: Vec<usize> = (0..servers.len())
        .filter(|&i| {
            let sv = &servers[i];
            query.is_empty()
                || sv.name.to_lowercase().contains(&query)
                || sv.map.to_lowercase().contains(&query)
                || sv.region.to_lowercase().contains(&query)
        })
        .collect();
    let list_y = 56.0 * s;
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
    if !servers.is_empty() && shown.is_empty() {
        let msg = "Aucun serveur ne correspond à la recherche.";
        let tw = Ui::text_width(msg, s);
        c.ui.text(
            list_x + (list_w - tw) / 2.0,
            list_y + list_h / 2.0 - 10.0 * s,
            s,
            GREY,
            msg,
        );
    }
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
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    for (row, &i) in shown.iter().take(visible).enumerate() {
        let sv = &servers[i];
        let y = top + row as f32 * row_h;
        let detail = detail(sv, now);
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
        let info = if sv.asleep {
            "endormi".to_string()
        } else if sv.max_players == 0 {
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
    if selected.is_some_and(|i| i >= servers.len() || !shown.contains(&i)) {
        *selected = None;
    }
    // Boutons : rejoindre, créer, actualiser, retour.
    let by = c.h - 48.0 * s;
    let cx = c.w / 2.0;
    let bw = 150.0;
    let half = (bw / 2.0 + 3.0) * s;
    // (v325) Un monde endormi se réveille d'ici, par son créateur ou celui qui en garde le
    // dernier état (« Ouvrir un monde » en fait un autre serveur).
    let pick = selected.and_then(|i| servers.get(i));
    let (label, can) = match pick {
        Some(sv) if sv.asleep => ("Réveiller", sv.can_wake),
        Some(_) => ("Rejoindre", true),
        None => ("Rejoindre", false),
    };
    let join = c.button(label, cx - half, by, bw, can);
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
    /// (v340) Certifiée (`/fps certif`, compte `proto::CERTIFIER`) : en tête de la liste.
    pub certified: bool,
}

/// (v340) Formats proposés par « Équipe » (joueurs par équipe) : une carte y apparaît quand
/// son nombre de joueurs (`/fps set maxplayer`) est le double. Seul le 5v5 est classé.
pub const TEAM_SIZES: [u8; 6] = [1, 2, 3, 4, 5, 10];

/// (v340) Format choisi par défaut, et le seul classé.
pub const RANKED_TEAM: u8 = 5;

/// (v340) « 5v5 ».
pub fn team_label(n: u8) -> String {
    format!("{n}v{n}")
}

/// (v340) Format valide (sinon le 5v5).
pub fn team_or_default(n: u8) -> u8 {
    if TEAM_SIZES.contains(&n) {
        n
    } else {
        RANKED_TEAM
    }
}

/// (v340) Les cartes montrées (indices dans `maps`) : celles du format `team` (joueurs par
/// équipe : `players` = 2 × `team`), dont le nom ou l'auteur contient `query`.
pub fn maps_for(maps: &[MapRow], team: u8, query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    (0..maps.len())
        .filter(|&i| maps[i].players.max(2) == 2 * team)
        .filter(|&i| {
            query.is_empty()
                || maps[i].name.to_lowercase().contains(&query)
                || maps[i].author.to_lowercase().contains(&query)
        })
        .collect()
}

/// (v340) Ordre de la liste des cartes : les certifiées d'abord, puis les plus jouées (à
/// parties égales, celles où l'on attend une partie, puis par nom).
pub fn sort_rows(rows: &mut [MapRow]) {
    rows.sort_by(|a, b| {
        b.certified
            .cmp(&a.certified)
            .then(b.plays.cmp(&a.plays))
            .then(b.waiting.cmp(&a.waiting))
            .then(a.name.cmp(&b.name))
    });
}

/// (v340) Un de ses mondes, proposé par « Ajouter une carte » : il part au relais et entre
/// dans la liste FPS de tous les joueurs.
#[derive(Clone, Debug)]
pub struct AddRow {
    pub dir: std::path::PathBuf,
    pub sig: [u8; 32],
    pub name: String,
    /// Joueurs d'une partie (format).
    pub players: u8,
    /// Départs CT et T réglés (`/fps set spawn`) : sinon, pas encore une carte FPS.
    pub configured: bool,
    /// Déjà dans la liste FPS du relais.
    pub listed: bool,
}

impl AddRow {
    pub fn can_add(&self) -> bool {
        self.configured && !self.listed
    }
}

/// Nom d'une carte sans sa version : (famille, version). « Overpass CS2 v.0.3.2 Quality »
/// donne (« overpass », [0, 3, 2]) ; « Aztec v_0_1 », (« aztec », [0, 1]). Les mots
/// d'édition (« CS2 », « NEW », « Quality »…) et une version de Minecraft (« 1.12.2 »)
/// ne comptent pas dans la famille.
pub fn map_family(name: &str) -> (String, Vec<u32>) {
    let mut version: Vec<u32> = Vec::new();
    let mut words: Vec<String> = Vec::new();
    for w in name.split_whitespace() {
        let l = w.to_lowercase();
        // Version : « v.0.3 », « v_0_1 », « v1.2 », « v4 ».
        if version.is_empty() {
            if let Some(rest) = l.strip_prefix('v') {
                let rest = rest.trim_start_matches(['.', '_']);
                let nums: Option<Vec<u32>> =
                    rest.split(['.', '_']).map(|p| p.parse().ok()).collect();
                if let Some(nums) = nums.filter(|n| !rest.is_empty() && !n.is_empty()) {
                    version = nums;
                    continue;
                }
            }
        }
        let edition = matches!(
            l.as_str(),
            "cs2" | "csgo" | "cs:go" | "new" | "quality" | "hd" | "remake" | "remastered"
        );
        let minecraft = l.contains('.') && l.chars().all(|c| c.is_ascii_digit() || c == '.');
        if !edition && !minecraft {
            words.push(l);
        }
    }
    (words.join(" "), version)
}

/// Anciennes versions d'une carte (même auteur, même nom à la version près) qui n'ont
/// jamais fini une partie en ligne : retirées de la liste. On garde de chaque carte sa
/// version la plus récente (à version égale : celle de ce PC, puis la plus jouée), toute
/// version qui a fini au moins une partie, celles où des joueurs attendent une partie, et
/// (v340) celles qui sont certifiées.
pub fn hide_old_versions(rows: &mut Vec<MapRow>) {
    let keys: Vec<(String, String, Vec<u32>)> = rows
        .iter()
        .map(|r| {
            let (family, version) = map_family(&r.name);
            (r.author.to_lowercase(), family, version)
        })
        .collect();
    let rank = |i: usize| {
        (
            keys[i].2.clone(),
            rows[i].local.is_some(),
            rows[i].plays,
            rows[i].stored,
        )
    };
    let mut best: std::collections::HashMap<(&str, &str), usize> = std::collections::HashMap::new();
    for (i, key) in keys.iter().enumerate() {
        let k = (key.0.as_str(), key.1.as_str());
        match best.get(&k) {
            Some(&j) if rank(j) >= rank(i) => {}
            _ => {
                best.insert(k, i);
            }
        }
    }
    let keep: Vec<bool> = (0..rows.len())
        .map(|i| {
            best.get(&(keys[i].0.as_str(), keys[i].1.as_str())) == Some(&i)
                || rows[i].plays > 0
                || rows[i].waiting > 0
                || rows[i].certified
        })
        .collect();
    let mut it = keep.into_iter();
    rows.retain(|_| it.next().unwrap_or(true));
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
    /// (v340) Format choisi (« Équipe »), en joueurs par équipe.
    Team(u8),
    /// (v340) « Ajouter une carte » : la liste de ses mondes est à refaire.
    OpenAdd,
    /// (v340) Ce monde (indice dans la liste de « Ajouter une carte ») part au relais.
    AddMap(usize),
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
    pub members_scroll: usize,
    /// Barre de défilement tenue : 1 classement, 2 cartes, 3 amis, 4 lobby, (v340) 5 ses
    /// mondes (« Ajouter une carte »).
    pub drag: u8,
    pub selected: Option<usize>,
    /// Recherche dans les cartes (à droite de « Carte »).
    pub map_search: String,
    pub map_search_focus: bool,
    /// Dernier clic sur une carte (index, horloge) : un double clic lance la partie.
    pub map_click: Option<(usize, f32)>,
    /// Dernière mise à jour automatique de la liste des cartes (horloge de l'écran).
    pub polled: f32,
    /// (v340) Format choisi (joueurs par équipe, `TEAM_SIZES` ; 0 : le 5v5), et sa liste
    /// ouverte.
    pub team: u8,
    pub team_open: bool,
    /// (v340) « Ajouter une carte » ouvert : monde choisi, défilement.
    pub adding: bool,
    pub add_selected: Option<usize>,
    pub add_scroll: usize,
}

impl FpsHome {
    /// (v340) Format choisi (le 5v5 tant que rien d'autre ne l'est).
    pub fn team(&self) -> u8 {
        team_or_default(self.team)
    }
}

/// Écran FPS : classement au-dessus, choix de la carte en dessous. (v340) En haut à gauche,
/// « Équipe : 5v5 » et sa liste (`TEAM_SIZES`) : seules les cartes de ce format sont
/// montrées ; « Ajouter une carte » met un de ses mondes (`adds`) dans la liste de tous.
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
    adds: &[AddRow],
    status: Option<&str>,
    busy: bool,
    time: f32,
    lobby: LobbyView,
) -> FpsAction {
    c.background();
    if st.adding {
        return add_screen(c, input, st, adds, status, busy, time);
    }
    let s = c.s;
    let ds = (s / 2.0).floor().max(1.0);
    // (v340) « Équipe : 5v5 », tout en haut à gauche. Sa liste ouverte passe par-dessus le
    // classement : ce qui est dessous ne voit ni la souris ni ses clics (rendus plus bas,
    // quand la liste est dessinée).
    let team = st.team();
    let drop = TeamDrop::at(s);
    let over_drop = c.hovered(drop.x, drop.y, drop.w, drop.h)
        || (st.team_open && c.hovered(drop.x, drop.y + drop.h, drop.w, drop.list_h()));
    let mouse = (c.cursor, c.click);
    if over_drop {
        c.cursor = None;
        c.click = false;
    }
    // Panneau du lobby à droite, le reste à gauche.
    let panel_w = (c.w * 0.26).clamp(120.0 * s, 170.0 * s);
    let avail = c.w - panel_w - 12.0 * s;
    // Couleurs proposées : celles d'une équipe de la carte choisie (5 sans carte).
    let colors = st
        .selected
        .and_then(|i| maps.get(i))
        .map_or(5, |m| (m.players.max(2) / 2) as usize)
        .clamp(1, PLAYER_COLORS.len());
    let panel_action = lobby_panel(c, input, st, &lobby, avail + 6.0 * s, panel_w, time, colors);
    let title = "FPS - compétitif";
    let tw = Ui::text_width(title, s * 1.5);
    // (v340) Jamais sur « Équipe : 5v5 » (en haut à gauche) : décalé à sa droite s'il le faut.
    let title_x = ((avail - tw) / 2.0).max(drop.x + drop.w + 8.0 * s);
    c.ui.text(title_x, 8.0 * s, s * 1.5, WHITE, title);
    let w = (avail - 16.0 * s).min(440.0 * s);
    let x = (avail - w) / 2.0;
    // Onglets du classement : les amis d'abord, le top mondial à droite.
    let tab_y = 24.0 * s;
    let tab_w = 110.0;
    // (v340) Pas de classement hors du 5v5 (pour le moment) : onglets éteints.
    let ranked = team == RANKED_TEAM;
    let friends = c.button(
        "Amis",
        x + tab_w * s / 2.0,
        tab_y,
        tab_w,
        ranked && st.world_tab,
    );
    let world = c.button(
        "Top mondial",
        x + tab_w * s * 1.5 + 4.0 * s,
        tab_y,
        tab_w,
        ranked && !st.world_tab,
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
    // Colonnes : le pseudo, puis sept nombres à intervalles égaux (la barre de défilement
    // à droite). Un nombre trop large pour sa colonne passe en petit (jamais collé au
    // suivant).
    let bar = c.bar_w();
    let cols: [(&str, f32); 9] = [
        ("#", 0.02),
        ("Joueur", 0.08),
        ("Elo", 0.33),
        ("Matchs", 0.42),
        ("Vict.", 0.51),
        ("K/D", 0.60),
        ("HS %", 0.69),
        ("ADR", 0.78),
        ("MVP", 0.87),
    ];
    let col_end = |k: usize| {
        cols.get(k + 1)
            .map_or(w - bar - 6.0 * s, |(_, f)| w * f - 3.0 * s)
    };
    for (label, f) in cols.into_iter().filter(|_| ranked) {
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
        for (k, ((_, f), v)) in cols.iter().zip(vals).enumerate() {
            let room = col_end(k) - w * f;
            if k == 1 {
                c.ui.text(x + w * f, y, s, color, crate::menu::fit(&v, room, s));
            } else if Ui::text_width(&v, s) <= room {
                c.ui.text(x + w * f, y, s, color, v);
            } else {
                c.ui.text(x + w * f, y + s, ds, color, v);
            }
        }
    };
    match board.filter(|b| b.0 != st.world_tab) {
        _ if !ranked => {
            let lines = [
                (
                    format!("Pas de classement en {} pour le moment.", team_label(team)),
                    s,
                ),
                (
                    format!("Seul le {} est classé.", team_label(RANKED_TEAM)),
                    ds,
                ),
            ];
            let mut ly = top + 16.0 * s;
            for (t, scale) in lines {
                let t = crate::menu::fit(&t, w - 8.0 * s, scale);
                c.ui.text(
                    x + (w - Ui::text_width(&t, scale)) / 2.0,
                    ly,
                    scale,
                    GREY,
                    t,
                );
                ly += 12.0 * s;
            }
        }
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
            Ctx::wheel(input, over, list.len(), visible, &mut st.scroll);
            let mut drag = st.drag == 1;
            c.scrollbar(
                x + w - bar - 3.0 * s,
                top,
                visible as f32 * row - s,
                list.len(),
                visible,
                &mut st.scroll,
                &mut drag,
            );
            st.drag = if drag {
                1
            } else if st.drag == 1 {
                0
            } else {
                st.drag
            };
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
    // Choix de la carte : « Carte » en grand, la recherche à sa droite.
    let my = list_y + list_h + 6.0 * s;
    let big = s * 1.5;
    c.ui.text(x, my + 4.0 * s, big, WHITE, "Carte");
    // Recherche alignée sur le bord droit de la liste.
    let after_label = x + Ui::text_width("Carte", big) + 8.0 * s;
    let field_w = ((x + w - after_label) / s).min(160.0);
    let field_x = x + w - field_w * s;
    if c.text_field(
        "",
        &st.map_search,
        "rechercher une carte",
        field_x + field_w * s / 2.0,
        my,
        field_w,
        st.map_search_focus,
        time,
    ) {
        st.map_search_focus = true;
        st.search_focus = false;
    } else if c.click {
        st.map_search_focus = false;
    }
    if st.map_search_focus {
        crate::menu::edit_text(&mut st.map_search, input, 24);
    }
    // (v340) Les cartes du format choisi (« Équipe »), dans l'ordre donné par le jeu (les
    // certifiées d'abord, puis les plus jouées : `sort_rows`).
    let fits_team = |m: &MapRow| m.players.max(2) == 2 * team;
    let shown = maps_for(maps, team, &st.map_search);
    if st
        .selected
        .is_some_and(|i| maps.get(i).is_none_or(|m| !fits_team(m)))
    {
        st.selected = None;
    }
    let maps_y = my + 24.0 * s;
    let maps_h = (c.h - maps_y - 56.0 * s).max(40.0 * s);
    c.frame(x, maps_y, w, maps_h);
    let row_h = 26.0 * s;
    let vis = ((maps_h - 4.0 * s) / row_h).floor().max(1.0) as usize;
    let over = c.hovered(x, maps_y, w, maps_h);
    Ctx::wheel(input, over, shown.len(), vis, &mut st.map_scroll);
    let mut drag = st.drag == 2;
    c.scrollbar(
        x + w - bar - 3.0 * s,
        maps_y + 2.0 * s,
        vis as f32 * row_h - s,
        shown.len(),
        vis,
        &mut st.map_scroll,
        &mut drag,
    );
    st.drag = if drag {
        2
    } else if st.drag == 2 {
        0
    } else {
        st.drag
    };
    // Place laissée à la barre quand elle est là.
    let right = if shown.len() > vis {
        bar + 4.0 * s
    } else {
        0.0
    };
    if shown.is_empty() {
        let lines = if maps.is_empty() {
            vec![
                "Aucune carte : crée un monde, règle les départs avec /fps set, et reviens."
                    .to_string(),
            ]
        } else if !maps.iter().any(fits_team) {
            vec![
                format!("Aucune carte en {}.", team_label(team)),
                format!(
                    "/fps set maxplayer {} dans un de tes mondes en fait une.",
                    2 * team
                ),
            ]
        } else {
            vec!["Aucune carte ne correspond à la recherche.".to_string()]
        };
        for (k, t) in lines.iter().enumerate() {
            let t = crate::menu::fit(t, w - 8.0 * s, ds);
            c.ui.text(
                x + (w - Ui::text_width(&t, ds)) / 2.0,
                maps_y + 14.0 * s + k as f32 * 11.0 * ds,
                ds,
                GREY,
                t,
            );
        }
    }
    let mut double = None;
    for (k, &i) in shown.iter().enumerate().skip(st.map_scroll).take(vis) {
        let m = &maps[i];
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
            w - 4.0 * s - right,
            row_h - s,
            &m.name,
            &detail,
            st.selected == Some(i),
        ) {
            st.selected = Some(i);
            if st.map_click.is_some_and(|(j, t)| j == i && time - t < 0.4) {
                double = Some(i);
                st.map_click = None;
            } else {
                st.map_click = Some((i, time));
            }
        }
        // Joueurs, bien visibles : « 3 / 10 » en attente, ou le format de la carte.
        let n = m.players.max(2) as u32;
        let (t, col) = if m.waiting > 0 {
            (format!("{} / {n}", m.waiting), GOLD)
        } else {
            (format!("{} contre {}", n / 2, n / 2), WHITE)
        };
        c.ui.text(
            x + w - Ui::text_width(&t, big) - 8.0 * s - right,
            y + 3.0 * s,
            big,
            col,
            t,
        );
        // Sous le format : (v340) « certifiée » en or, et « hors classement » hors du 5v5.
        let mut right_x = x + w - 8.0 * s - right;
        if n != 10 {
            let r = "hors classement";
            right_x -= Ui::text_width(r, ds);
            c.ui.text(right_x, y + 16.0 * s, ds, GREY, r);
            right_x -= 4.0 * ds;
        }
        if m.certified {
            let r = "certifiée";
            right_x -= Ui::text_width(r, ds);
            c.ui.text(right_x, y + 16.0 * s, ds, GOLD, r);
        }
    }
    if let Some(t) = status {
        let t = crate::menu::fit(t, avail - 8.0 * s, s);
        let tw = Ui::text_width(&t, s);
        c.ui.text((avail - tw) / 2.0, c.h - 60.0 * s, s, GOLD, t);
    }
    // (v340) La liste des formats, par-dessus le reste : la souris lui revient.
    (c.cursor, c.click) = mouse;
    let esc = input.pressed(minelib_platform::KeyCode::Escape);
    // Échap ferme la liste ouverte (sans quitter l'écran).
    let was_open = st.team_open;
    if let Some(n) = team_drop(c, st, &drop, esc) {
        st.team = n;
        st.map_scroll = 0;
        action = FpsAction::Team(n);
    }
    let esc = esc && !was_open;
    let by = c.h - 48.0 * s;
    let cx = avail / 2.0;
    let half = (150.0 / 2.0 + 3.0) * s;
    // En lobby, seul son hôte lance (pour tout le monde).
    let leader = lobby
        .members
        .first()
        .is_none_or(|m| m.eq_ignore_ascii_case(lobby.me));
    let label = if leader { "Jouer" } else { "L'hôte lance" };
    // Le lobby joue dans la même équipe : il doit tenir dans la moitié de la carte.
    let team = st
        .selected
        .and_then(|i| maps.get(i))
        .map_or(usize::MAX, |m| (m.players.max(2) / 2) as usize);
    let fits = lobby.members.len() <= team;
    if !fits && status.is_none() {
        let t = format!(
            "Lobby de {} joueurs : cette carte n'en prend que {team} par équipe.",
            lobby.members.len()
        );
        let t = crate::menu::fit(&t, avail - 8.0 * s, s);
        let tw = Ui::text_width(&t, s);
        c.ui.text((avail - tw) / 2.0, c.h - 60.0 * s, s, crate::menu::RED, t);
    }
    let search = c.button(
        label,
        cx - half,
        by,
        150.0,
        st.selected.is_some() && !busy && leader && fits,
    );
    // (v340) « Ajouter une carte » : un de ses mondes dans la liste FPS de tous.
    let add = c.button("Ajouter une carte", cx + half, by, 150.0, !busy);
    let refresh = c.button("Actualiser", cx - half, by + 24.0 * s, 150.0, true);
    let back = c.button("Retour", cx + half, by + 24.0 * s, 150.0, true);
    if back || esc {
        return FpsAction::Back;
    }
    if add {
        st.adding = true;
        st.add_selected = None;
        st.add_scroll = 0;
        st.team_open = false;
        return FpsAction::OpenAdd;
    }
    if let (true, Some(i)) = (search, st.selected) {
        return FpsAction::Search(i);
    }
    // Double clic sur une carte : comme « Jouer » (si on peut jouer).
    if let Some(i) = double.filter(|_| !busy && leader && fits) {
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

/// (v340) Où est « Équipe : 5v5 » (tout en haut à gauche de l'écran FPS), et sa liste.
struct TeamDrop {
    s: f32,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    row: f32,
}

impl TeamDrop {
    fn at(s: f32) -> Self {
        // Assez large pour le plus long (« Équipe : 10v10 ») et la flèche.
        let w = TEAM_SIZES
            .iter()
            .map(|&n| Ui::text_width(&team_text(n), s))
            .fold(0.0, f32::max)
            + 22.0 * s;
        Self {
            s,
            x: 6.0 * s,
            y: 5.0 * s,
            w,
            h: 14.0 * s,
            row: 12.0 * s,
        }
    }

    fn list_h(&self) -> f32 {
        TEAM_SIZES.len() as f32 * self.row + 4.0 * self.s
    }
}

/// (v340) « Équipe : 5v5 ».
fn team_text(n: u8) -> String {
    format!("Équipe : {}", team_label(n))
}

/// (v340) Petite flèche vers le bas (7 × 4 pixels de texture).
fn down_arrow(c: &mut Ctx, x: f32, y: f32, s: f32, color: [f32; 4]) {
    for k in 0..4 {
        let k = k as f32;
        c.ui.rect(x + k * s, y + k * s, (7.0 - 2.0 * k) * s, s, color);
    }
}

/// (v340) Dessine « Équipe : 5v5 » et, ouverte, la liste des formats ; un clic sur le cadre
/// l'ouvre ou la ferme, un clic ailleurs (ou Échap) la ferme. Rend le format choisi.
fn team_drop(c: &mut Ctx, st: &mut FpsHome, d: &TeamDrop, esc: bool) -> Option<u8> {
    let s = d.s;
    let team = st.team();
    let hover = c.hovered(d.x, d.y, d.w, d.h);
    let lit = hover || st.team_open;
    c.frame(d.x, d.y, d.w, d.h);
    if lit {
        c.ui.rect(
            d.x + s,
            d.y + s,
            d.w - 2.0 * s,
            d.h - 2.0 * s,
            [1.0, 1.0, 1.0, 0.08],
        );
    }
    let ty = d.y + (d.h - 8.0 * s) / 2.0;
    c.ui.text(d.x + 5.0 * s, ty, s, WHITE, team_text(team));
    down_arrow(
        c,
        d.x + d.w - 11.0 * s,
        d.y + (d.h - 4.0 * s) / 2.0,
        s,
        if lit { GOLD } else { WHITE },
    );
    let mut picked = None;
    if st.team_open {
        let ly = d.y + d.h;
        // Fond plein : rien de ce qui est dessous ne se voit à travers.
        c.ui.rect(d.x, ly, d.w, d.list_h(), [0.11, 0.12, 0.15, 1.0]);
        c.frame(d.x, ly, d.w, d.list_h());
        for (k, &n) in TEAM_SIZES.iter().enumerate() {
            let ry = ly + 2.0 * s + k as f32 * d.row;
            let over = c.hovered(d.x, ry, d.w, d.row);
            if n == team || over {
                let a = if n == team { 0.12 } else { 0.07 };
                c.ui.rect(d.x + s, ry, d.w - 2.0 * s, d.row, [1.0, 1.0, 1.0, a]);
            }
            let col = if n == team { GOLD } else { WHITE };
            let mut label = team_label(n);
            if n == RANKED_TEAM {
                label += " (classé)";
            }
            c.ui.text(d.x + 6.0 * s, ry + (d.row - 8.0 * s) / 2.0, s, col, label);
            if over && c.click {
                picked = Some(n);
            }
        }
    }
    if c.click {
        // Le cadre ouvre ou ferme ; un format choisi, ou un clic ailleurs, ferme.
        st.team_open = hover && !st.team_open;
    } else if esc {
        st.team_open = false;
    }
    picked
}

/// (v340) « Ajouter une carte » : ses mondes (`adds`, rangés par le jeu) ; celui choisi part
/// au relais et entre dans la liste FPS de tous les joueurs, comme une carte lancée une fois
/// en ligne. Il faut que ses départs soient réglés, et qu'il n'y soit pas déjà.
fn add_screen(
    c: &mut Ctx,
    input: &minelib_platform::Input,
    st: &mut FpsHome,
    adds: &[AddRow],
    status: Option<&str>,
    busy: bool,
    time: f32,
) -> FpsAction {
    let s = c.s;
    let ds = (s / 2.0).floor().max(1.0);
    c.text_center("Ajouter une carte", 10.0 * s, s * 1.5, WHITE);
    let w = (c.w - 40.0 * s).min(400.0 * s);
    let x = (c.w - w) / 2.0;
    // Ce qu'on fait ici, sur autant de lignes qu'il faut.
    let note = "Choisis un de tes mondes dont les départs sont réglés (/fps set spawn ct et \
                /fps set spawn t) : il part au relais, et tous les joueurs le voient dans la \
                liste des cartes FPS.";
    let lines = crate::friends::wrap(note, w, ds);
    for (k, l) in lines.iter().enumerate() {
        let lw = Ui::text_width(l, ds);
        c.ui.text(
            (c.w - lw) / 2.0,
            28.0 * s + k as f32 * 10.0 * ds,
            ds,
            GREY,
            l.as_str(),
        );
    }
    let list_y = 32.0 * s + lines.len() as f32 * 10.0 * ds;
    let list_h = (c.h - list_y - 64.0 * s).max(40.0 * s);
    c.frame(x, list_y, w, list_h);
    let row_h = 26.0 * s;
    let vis = ((list_h - 4.0 * s) / row_h).floor().max(1.0) as usize;
    let over = c.hovered(x, list_y, w, list_h);
    Ctx::wheel(input, over, adds.len(), vis, &mut st.add_scroll);
    let bar = c.bar_w();
    let mut drag = st.drag == 5;
    c.scrollbar(
        x + w - bar - 3.0 * s,
        list_y + 2.0 * s,
        vis as f32 * row_h - s,
        adds.len(),
        vis,
        &mut st.add_scroll,
        &mut drag,
    );
    st.drag = if drag {
        5
    } else if st.drag == 5 {
        0
    } else {
        st.drag
    };
    let right = if adds.len() > vis { bar + 4.0 * s } else { 0.0 };
    if adds.is_empty() {
        let t = "Aucun monde : crée-en un, règle ses départs avec /fps set spawn, et reviens.";
        let t = crate::menu::fit(t, w - 8.0 * s, ds);
        c.ui.text(
            x + (w - Ui::text_width(&t, ds)) / 2.0,
            list_y + 14.0 * s,
            ds,
            GREY,
            t,
        );
    }
    if st.add_selected.is_some_and(|i| i >= adds.len()) {
        st.add_selected = None;
    }
    let mut double = None;
    for (k, a) in adds.iter().enumerate().skip(st.add_scroll).take(vis) {
        let y = list_y + 2.0 * s + (k - st.add_scroll) as f32 * row_h;
        let n = a.players.max(2) / 2;
        let state = if a.listed {
            "déjà dans la liste FPS"
        } else if !a.configured {
            "départs non réglés (/fps set spawn)"
        } else {
            "prête à ajouter"
        };
        let detail = format!("{} - {state}", team_label(n));
        if c.list_row(
            x + 2.0 * s,
            y,
            w - 4.0 * s - right,
            row_h - s,
            &a.name,
            &detail,
            st.add_selected == Some(k),
        ) {
            if st.add_selected == Some(k)
                && st.map_click.is_some_and(|(j, t)| j == k && time - t < 0.4)
            {
                double = Some(k);
                st.map_click = None;
            } else {
                st.map_click = Some((k, time));
            }
            st.add_selected = Some(k);
        }
    }
    if let Some(t) = status {
        let t = crate::menu::fit(t, c.w - 16.0 * s, s);
        let tw = Ui::text_width(&t, s);
        c.ui.text((c.w - tw) / 2.0, c.h - 60.0 * s, s, GOLD, t);
    }
    let by = c.h - 48.0 * s;
    let half = (150.0 / 2.0 + 3.0) * s;
    let pick = st
        .add_selected
        .filter(|&i| adds.get(i).is_some_and(AddRow::can_add) && !busy);
    let add = c.button("Ajouter", c.w / 2.0 - half, by, 150.0, pick.is_some());
    let cancel = c.button("Annuler", c.w / 2.0 + half, by, 150.0, true);
    let chosen = if add {
        pick
    } else {
        double.filter(|_| pick == double)
    };
    if let Some(i) = chosen {
        st.adding = false;
        st.map_click = None;
        return FpsAction::AddMap(i);
    }
    if cancel || input.pressed(minelib_platform::KeyCode::Escape) {
        st.adding = false;
        st.map_click = None;
    }
    FpsAction::None
}

/// Couleurs des joueurs d'une équipe (lobby, bandeau des têtes, triangles, carte), dans
/// l'ordre où elles servent : une partie à N contre N n'utilise que les N premières (les
/// 5 premières pour le 5 contre 5, les 2 premières pour le 2 contre 2). Teintes choisies
/// pour rester distinctes pour les daltoniens : aux simulations de protanopie, deutéranopie
/// et tritanopie (Machado 2009), l'écart CIEDE2000 entre deux couleurs est d'au moins 15
/// parmi les 5 premières, 12 parmi les 8 premières et 10 parmi les 10.
pub const PLAYER_COLORS: [[f32; 4]; 10] = [
    rgb(0x21, 0x8D, 0xDA), // bleu vif
    rgb(0xF2, 0x61, 0x00), // orange vif
    rgb(0x2B, 0xD8, 0x82), // vert vif
    rgb(0xFF, 0xF6, 0x11), // jaune vif
    rgb(0x5D, 0x2B, 0x90), // violet foncé
    rgb(0xC7, 0x20, 0x07), // rouge vif
    rgb(0xFF, 0xB2, 0xC1), // rose flamant rose
    rgb(0xFF, 0xFF, 0xFF), // blanc
    rgb(0x99, 0x99, 0x99), // gris clair
    rgb(0x59, 0x26, 0x10), // marron foncé
];

const fn rgb(r: u8, g: u8, b: u8) -> [f32; 4] {
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0]
}

/// Panneau du lobby, à droite : recherche d'amis en haut, amis en dessous (clic : inviter),
/// invitations reçues, et les membres du lobby en bas (clic droit : exclure, promouvoir).
/// `colors` : couleurs proposées (celles d'une équipe de la carte choisie).
#[allow(clippy::too_many_arguments)]
fn lobby_panel(
    c: &mut Ctx,
    input: &minelib_platform::Input,
    st: &mut FpsHome,
    v: &LobbyView,
    x: f32,
    w: f32,
    time: f32,
    colors: usize,
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
        st.map_search_focus = false;
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
    let sq = 8.0 * s;
    let step = sq + 2.0 * s;
    let cx0 = lx + Ui::text_width("Ta couleur", ds) + 4.0 * s;
    // Autant de carrés par ligne que la place le permet (10 couleurs : souvent 2 lignes).
    let per = (((lx + inner - cx0 + 2.0 * s) / step).floor() as usize).max(1);
    let lines = colors.div_ceil(per);
    for (k, col) in PLAYER_COLORS.iter().enumerate().take(colors) {
        let bx = cx0 + (k % per) as f32 * step;
        let cy = cy + (k / per) as f32 * step;
        let chosen = v.color == k as u8;
        if chosen {
            c.ui.rect(bx - s, cy - s, sq + 2.0 * s, sq + 2.0 * s, GOLD);
        }
        c.ui.rect(bx, cy, sq, sq, *col);
        if c.hovered(bx, cy, sq, sq) && c.click {
            out = FpsAction::Color(if chosen { 255 } else { k as u8 });
        }
    }
    let mut y = cy + lines as f32 * step + 4.0 * s;
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
    // Membres du lobby, en bas : 5 places à l'écran (une de libre de plus tant qu'il en
    // reste, jusqu'à 10 : la barre de défilement fait voir les autres).
    let slot_h = 14.0 * s;
    const SLOTS_SHOWN: usize = 5;
    let slots = (v.members.len() + 1).clamp(SLOTS_SHOWN, proto::PARTY_MAX);
    let members_h = 12.0 * s + slot_h * SLOTS_SHOWN as f32 + 24.0 * s;
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
    let over = c.hovered(x, y, w, list_h);
    Ctx::wheel(input, over, list.len(), vis, &mut st.friends_scroll);
    let bar = c.bar_w();
    let mut drag = st.drag == 3;
    c.scrollbar(
        lx + inner - bar,
        y,
        vis as f32 * row - s,
        list.len(),
        vis,
        &mut st.friends_scroll,
        &mut drag,
    );
    st.drag = if drag {
        3
    } else if st.drag == 3 {
        0
    } else {
        st.drag
    };
    // Les lignes laissent la place à la barre quand elle est là.
    let inner_f = if list.len() > vis {
        inner - bar - 2.0 * s
    } else {
        inner
    };
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
        let hover = c.hovered(lx, ry, inner_f, row);
        let can = *on && !in_lobby(p) && v.members.len() < proto::PARTY_MAX;
        if hover && can {
            c.ui.rect(lx, ry, inner_f, row, [1.0, 1.0, 1.0, 0.08]);
        }
        let dot = if *on {
            [0.3, 0.85, 0.35, 1.0]
        } else {
            [0.45, 0.45, 0.45, 1.0]
        };
        c.ui.rect(lx + 2.0 * s, ry + 4.0 * s, 4.0 * s, 4.0 * s, dot);
        let hint = if hover && can { "inviter" } else { "" };
        let hw = Ui::text_width(hint, ds);
        let name = fit(p, inner_f - 12.0 * s - hw, s);
        c.ui.text(
            lx + 9.0 * s,
            ry + 2.0 * s,
            s,
            if *on { WHITE } else { GREY },
            name,
        );
        if !hint.is_empty() {
            c.ui.text(lx + inner_f - hw - 2.0 * s, ry + 4.0 * s, ds, GOLD, hint);
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
    let slots_y = members_y + 10.0 * s;
    let over = c.hovered(lx, slots_y, inner, slot_h * SLOTS_SHOWN as f32);
    Ctx::wheel(input, over, slots, SLOTS_SHOWN, &mut st.members_scroll);
    let mut drag = st.drag == 4;
    c.scrollbar(
        lx + inner - bar,
        slots_y,
        slot_h * SLOTS_SHOWN as f32 - s,
        slots,
        SLOTS_SHOWN,
        &mut st.members_scroll,
        &mut drag,
    );
    st.drag = if drag {
        4
    } else if st.drag == 4 {
        0
    } else {
        st.drag
    };
    let inner_m = if slots > SLOTS_SHOWN {
        inner - bar - 2.0 * s
    } else {
        inner
    };
    for k in (st.members_scroll..slots).take(SLOTS_SHOWN) {
        let col = PLAYER_COLORS[k % PLAYER_COLORS.len()];
        let sy = slots_y + (k - st.members_scroll) as f32 * slot_h;
        c.ui.rect(lx, sy, inner_m, slot_h - s, [0.0, 0.0, 0.0, 0.25]);
        let Some(p) = shown.get(k) else {
            c.ui.text(lx + 9.0 * s, sy + 3.0 * s, ds, GREY, "place libre");
            continue;
        };
        c.ui.rect(lx + 2.0 * s, sy + 3.0 * s, 4.0 * s, 6.0 * s, col);
        let tag = if k == 0 && v.members.len() > 1 {
            " (hôte)"
        } else {
            ""
        };
        let t = fit(&format!("{p}{tag}"), inner_m - 12.0 * s, s);
        let me = p.eq_ignore_ascii_case(v.me);
        c.ui.text(
            lx + 9.0 * s,
            sy + 2.0 * s,
            s,
            if me { GOLD } else { WHITE },
            t,
        );
        if right && c.hovered(lx, sy, inner_m, slot_h) && i_lead && !me {
            if let Some((mx, my)) = c.cursor {
                st.member_menu = Some((p.clone(), mx, my));
            }
        }
    }
    if v.members.len() > 1 {
        let ly = members_y + 12.0 * s + slot_h * SLOTS_SHOWN as f32;
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
        c.ui.text(
            mx + 4.0 * s,
            my + 2.0 * s,
            ds,
            GREY,
            fit(&p, mw - 8.0 * s, ds),
        );
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

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, author: &str, local: bool, plays: u32) -> MapRow {
        MapRow {
            sig: [name.len() as u8; 32],
            name: name.to_string(),
            author: author.to_string(),
            local: local.then(|| std::path::PathBuf::from(name)),
            waiting: 0,
            plays,
            configured: true,
            players: 10,
            stored: true,
            certified: false,
        }
    }

    /// (v311) La fiche d'un monde : record et dernière activité ; endormi, chez qui est
    /// son dernier état.
    #[test]
    fn a_world_row_tells_its_record_its_last_activity_and_who_keeps_it() {
        let mut sv = ServerInfo {
            name: "L'île".into(),
            map: "ile".into(),
            players: 2,
            max_players: 0,
            ping: 0,
            region: "Relais".into(),
            peak: 9,
            active: 10_000,
            asleep: false,
            keeper: String::new(),
            can_wake: false,
        };
        assert_eq!(
            detail(&sv, 10_030),
            "ile - Relais - record 9 - actif à l'instant"
        );
        sv.players = 9;
        assert_eq!(
            detail(&sv, 10_000 + 600),
            "ile - Relais - actif il y a 10 min"
        );
        sv.asleep = true;
        sv.keeper = "Bob".into();
        assert_eq!(
            detail(&sv, 10_000 + 3 * 86_400),
            "ile - endormi, il y a 3 j - record 9 - dernier état chez Bob"
        );
        assert_eq!(ago(7200, 0), "il y a 2 h");
        sv.active = 0;
        sv.peak = 0;
        sv.keeper.clear();
        assert_eq!(detail(&sv, 5), "ile - endormi");
    }

    #[test]
    fn map_names_split_into_family_and_version() {
        assert_eq!(
            map_family("Overpass CS2 v.0.3.2 Quality"),
            ("overpass".to_string(), vec![0, 3, 2])
        );
        assert_eq!(
            map_family("Overpass v.0.1"),
            ("overpass".to_string(), vec![0, 1])
        );
        assert_eq!(map_family("Aztec v_0_1"), ("aztec".to_string(), vec![0, 1]));
        assert_eq!(
            map_family("CACHE NEW CS2 v.0.5 Quality"),
            ("cache".to_string(), vec![0, 5])
        );
        assert_eq!(
            map_family("Dust II v.0.3 1.12.2"),
            ("dust ii".to_string(), vec![0, 3])
        );
        assert_eq!(
            map_family("Vertigo R v.4.0"),
            ("vertigo r".to_string(), vec![4, 0])
        );
        // Un monde numéroté n'est pas une version.
        assert_eq!(
            map_family("Nouveau monde 18"),
            ("nouveau monde 18".to_string(), vec![])
        );
    }

    #[test]
    fn old_versions_never_played_online_leave_the_list() {
        let mut rows = vec![
            row("Overpass v.0.1", "viendax", true, 0),
            row("Overpass CS2 v.0.3.2 Quality", "viendax", true, 0),
            row("Cache CS2 v.0.2.1", "viendax", true, 3),
            row("CACHE NEW CS2 v.0.5 Quality", "viendax", true, 0),
            // La même carte qu'une autre personne a faite : pas une version de la mienne.
            row("Overpass v.0.1", "Fox", false, 0),
            row("Nouveau monde 1", "viendax", true, 0),
            row("Nouveau monde 2", "viendax", true, 0),
        ];
        // Ancienne signature du même monde (sur le relais), jamais finie en ligne.
        let mut stale = row("Overpass CS2 v.0.3.2 Quality", "viendax", false, 0);
        stale.sig = [99; 32];
        rows.push(stale);
        // Ancienne version où des joueurs attendent une partie : gardée.
        let mut waited = row("Overpass v.0.0.9", "viendax", false, 0);
        waited.waiting = 2;
        rows.push(waited);
        // (v340) Ancienne version certifiée : gardée.
        let mut certified = row("Overpass v.0.0.5", "viendax", false, 0);
        certified.sig = [98; 32];
        certified.certified = true;
        rows.push(certified);
        hide_old_versions(&mut rows);
        let names: Vec<(String, String, bool)> = rows
            .iter()
            .map(|r| (r.name.clone(), r.author.clone(), r.local.is_some()))
            .collect();
        let has = |n: &str, a: &str, local: bool| {
            names.iter().any(|x| x.0 == n && x.1 == a && x.2 == local)
        };
        assert!(
            has("Overpass CS2 v.0.3.2 Quality", "viendax", true),
            "{names:?}"
        );
        assert!(!has("Overpass v.0.1", "viendax", true), "{names:?}");
        assert!(
            !has("Overpass CS2 v.0.3.2 Quality", "viendax", false),
            "{names:?}"
        );
        assert!(has("Overpass v.0.0.9", "viendax", false), "{names:?}");
        assert!(
            has("Cache CS2 v.0.2.1", "viendax", true),
            "jouée en ligne : {names:?}"
        );
        assert!(
            has("CACHE NEW CS2 v.0.5 Quality", "viendax", true),
            "{names:?}"
        );
        assert!(has("Overpass v.0.1", "Fox", false), "{names:?}");
        assert!(has("Nouveau monde 1", "viendax", true) && has("Nouveau monde 2", "viendax", true));
        assert!(
            has("Overpass v.0.0.5", "viendax", false),
            "certifiée : {names:?}"
        );
        assert_eq!(rows.len(), 8, "{names:?}");
    }

    /// (v340) « Équipe : 2v2 » : les cartes pour 4 joueurs seulement ; la recherche filtre
    /// en plus. Les certifiées d'abord, puis les plus jouées.
    #[test]
    fn the_team_format_picks_the_maps_and_certified_ones_come_first() {
        let mut rows = vec![
            row("Dust", "a", false, 50),
            row("Petite", "b", false, 3),
            row("Mirage", "c", false, 80),
            row("Duel", "d", false, 9),
            row("Grande", "e", false, 1),
            row("Inferno", "f", false, 2),
        ];
        rows[1].players = 4;
        rows[3].players = 2;
        rows[4].players = 20;
        rows[5].certified = true;
        rows[0].waiting = 4;
        sort_rows(&mut rows);
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            ["Inferno", "Mirage", "Dust", "Duel", "Petite", "Grande"]
        );
        let pick = |team: u8, q: &str| -> Vec<&str> {
            maps_for(&rows, team, q)
                .into_iter()
                .map(|i| rows[i].name.as_str())
                .collect()
        };
        assert_eq!(pick(5, ""), ["Inferno", "Mirage", "Dust"]);
        assert_eq!(pick(2, ""), ["Petite"]);
        assert_eq!(pick(1, ""), ["Duel"]);
        assert_eq!(pick(10, ""), ["Grande"]);
        assert!(pick(3, "").is_empty());
        assert_eq!(pick(5, "mir"), ["Mirage"]);
        // Un format inconnu : le 5v5.
        assert_eq!(team_or_default(7), RANKED_TEAM);
        assert_eq!(team_or_default(10), 10);
        assert_eq!(team_label(10), "10v10");
    }
}
