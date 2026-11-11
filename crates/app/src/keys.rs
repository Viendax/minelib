//! Touches du jeu, réassignables (écran « Régler les touches »), enregistrées dans
//! `.minelib/keys.txt`. Ce sont des touches **physiques** : la même place sur le clavier
//! en AZERTY et en QWERTY ; le nom affiché suit la disposition réelle du clavier.

use std::fs;
use std::path::Path;

use minelib_platform::{Input, KeyCode, Mouse};

/// Touche du clavier ou bouton de la souris.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bind {
    Key(KeyCode),
    Mouse(Mouse),
}

impl Bind {
    pub fn held(self, input: &Input) -> bool {
        match self {
            Bind::Key(k) => input.held(k),
            Bind::Mouse(b) => input.mouse_held(b),
        }
    }

    pub fn pressed(self, input: &Input) -> bool {
        match self {
            Bind::Key(k) => input.pressed(k),
            Bind::Mouse(b) => input.mouse_pressed(b),
        }
    }

    fn to_name(self) -> String {
        match self {
            Bind::Key(k) => format!("{k:?}"),
            Bind::Mouse(b) => format!("Mouse{}", b as u8),
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        match name {
            "Mouse0" => Some(Bind::Mouse(Mouse::Left)),
            "Mouse1" => Some(Bind::Mouse(Mouse::Right)),
            "Mouse2" => Some(Bind::Mouse(Mouse::Middle)),
            _ => key_from_name(name).map(Bind::Key),
        }
    }

    pub fn name(self, input: &Input) -> String {
        match self {
            Bind::Key(k) => key_name(k, input),
            Bind::Mouse(Mouse::Left) => "Clic gauche".into(),
            Bind::Mouse(Mouse::Right) => "Clic droit".into(),
            Bind::Mouse(Mouse::Middle) => "Clic molette".into(),
        }
    }
}

/// Groupe d'une action : onglet « Build » (modes de construction), onglet « FPS », ou
/// les deux (actions communes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Build,
    Fps,
    Shared,
}

impl Group {
    /// Deux actions de ces groupes peuvent-elles se gêner (même touche) ?
    fn overlaps(self, other: Group) -> bool {
        self == other || self == Group::Shared || other == Group::Shared
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Attack,
    Use,
    Pick,
    Forward,
    Back,
    Left,
    Right,
    Jump,
    Sneak,
    Sprint,
    Inventory,
    Chat,
    Zoom,
    DayNight,
    Command,
    Debug,
    Screenshot,
    FpsForward,
    FpsBack,
    FpsLeft,
    FpsRight,
    FpsJump,
    FpsDuck,
    FpsWalk,
    FpsFire,
    FpsScope,
    FpsReload,
    FpsUse,
    FpsInspect,
    FpsDrop,
    FpsBuy,
    FpsAutoBuy,
    FpsRebuy,
    FpsPrimary,
    FpsSecondary,
    FpsKnife,
    FpsGrenade,
    FpsBomb,
    FpsLastWeapon,
    FpsNextWeapon,
    FpsPrevWeapon,
    FpsHe,
    FpsFlash,
    FpsSmoke,
    FpsDecoy,
    FpsMolotov,
    FpsScoreboard,
    FpsChat,
    FpsTeamChat,
    FpsRadio1,
    FpsRadio2,
    FpsRadio3,
    FpsVoice,
    FpsSpray,
    FpsPing,
}

impl Action {
    pub const ALL: [Action; 55] = [
        Action::Attack,
        Action::Use,
        Action::Pick,
        Action::Forward,
        Action::Back,
        Action::Left,
        Action::Right,
        Action::Jump,
        Action::Sneak,
        Action::Sprint,
        Action::Inventory,
        Action::Chat,
        Action::Zoom,
        Action::DayNight,
        Action::Command,
        Action::Debug,
        Action::Screenshot,
        Action::FpsForward,
        Action::FpsBack,
        Action::FpsLeft,
        Action::FpsRight,
        Action::FpsJump,
        Action::FpsDuck,
        Action::FpsWalk,
        Action::FpsFire,
        Action::FpsScope,
        Action::FpsReload,
        Action::FpsUse,
        Action::FpsInspect,
        Action::FpsDrop,
        Action::FpsBuy,
        Action::FpsAutoBuy,
        Action::FpsRebuy,
        Action::FpsPrimary,
        Action::FpsSecondary,
        Action::FpsKnife,
        Action::FpsGrenade,
        Action::FpsBomb,
        Action::FpsLastWeapon,
        Action::FpsNextWeapon,
        Action::FpsPrevWeapon,
        Action::FpsHe,
        Action::FpsFlash,
        Action::FpsSmoke,
        Action::FpsDecoy,
        Action::FpsMolotov,
        Action::FpsScoreboard,
        Action::FpsChat,
        Action::FpsTeamChat,
        Action::FpsRadio1,
        Action::FpsRadio2,
        Action::FpsRadio3,
        Action::FpsVoice,
        Action::FpsSpray,
        Action::FpsPing,
    ];

    pub fn group(self) -> Group {
        match self {
            Action::Attack
            | Action::Use
            | Action::Pick
            | Action::Forward
            | Action::Back
            | Action::Left
            | Action::Right
            | Action::Jump
            | Action::Sneak
            | Action::Sprint
            | Action::Inventory
            | Action::Chat
            | Action::Zoom
            | Action::DayNight => Group::Build,
            Action::Command | Action::Debug | Action::Screenshot => Group::Shared,
            Action::FpsForward
            | Action::FpsBack
            | Action::FpsLeft
            | Action::FpsRight
            | Action::FpsJump
            | Action::FpsDuck
            | Action::FpsWalk
            | Action::FpsFire
            | Action::FpsScope
            | Action::FpsReload
            | Action::FpsUse
            | Action::FpsInspect
            | Action::FpsDrop
            | Action::FpsBuy
            | Action::FpsAutoBuy
            | Action::FpsRebuy
            | Action::FpsPrimary
            | Action::FpsSecondary
            | Action::FpsKnife
            | Action::FpsGrenade
            | Action::FpsBomb
            | Action::FpsLastWeapon
            | Action::FpsNextWeapon
            | Action::FpsPrevWeapon
            | Action::FpsHe
            | Action::FpsFlash
            | Action::FpsSmoke
            | Action::FpsDecoy
            | Action::FpsMolotov
            | Action::FpsScoreboard
            | Action::FpsChat
            | Action::FpsTeamChat
            | Action::FpsRadio1
            | Action::FpsRadio2
            | Action::FpsRadio3
            | Action::FpsVoice
            | Action::FpsSpray
            | Action::FpsPing => Group::Fps,
        }
    }

    /// Actions d'un onglet du menu des touches (les communes sont dans les deux).
    pub fn tab(fps: bool) -> Vec<Action> {
        let g = if fps { Group::Fps } else { Group::Build };
        // Actions de l'onglet, puis les actions communes.
        let own = Self::ALL.into_iter().filter(|a| a.group() == g);
        own.chain(Self::ALL.into_iter().filter(|a| a.group() == Group::Shared))
            .collect()
    }

    /// Action pas encore présente dans le jeu (affichée en gris) : la touche est gardée.
    pub fn coming_soon(self) -> bool {
        matches!(
            self,
            Action::FpsUse
                | Action::FpsInspect
                | Action::FpsBomb
                | Action::FpsScoreboard
                | Action::FpsRadio1
                | Action::FpsRadio2
                | Action::FpsRadio3
                | Action::FpsVoice
                | Action::FpsSpray
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            Action::Attack => "Casser",
            Action::Use => "Poser",
            Action::Pick => "Copier le bloc",
            Action::Forward => "Avancer",
            Action::Back => "Reculer",
            Action::Left => "Gauche",
            Action::Right => "Droite",
            Action::Jump => "Sauter / monter",
            Action::Sneak => "S'accroupir",
            Action::Sprint => "Courir",
            Action::Inventory => "Inventaire",
            Action::Chat => "Tchat",
            Action::Zoom => "Zoom",
            Action::DayNight => "Jour / nuit",
            Action::Command => "Commande",
            Action::Debug => "Debug (F3)",
            Action::Screenshot => "Capture d'écran",
            Action::FpsForward => "Avancer",
            Action::FpsBack => "Reculer",
            Action::FpsLeft => "Gauche",
            Action::FpsRight => "Droite",
            Action::FpsJump => "Sauter",
            Action::FpsDuck => "S'accroupir",
            Action::FpsWalk => "Marcher",
            Action::FpsFire => "Tirer",
            Action::FpsScope => "Lunette / zoom",
            Action::FpsReload => "Recharger",
            Action::FpsUse => "Utiliser",
            Action::FpsInspect => "Inspecter",
            Action::FpsDrop => "Lâcher l'arme",
            Action::FpsBuy => "Menu d'achat",
            Action::FpsAutoBuy => "Achat auto",
            Action::FpsRebuy => "Racheter",
            Action::FpsPrimary => "Arme principale",
            Action::FpsSecondary => "Arme secondaire",
            Action::FpsKnife => "Couteau",
            Action::FpsGrenade => "Grenades",
            Action::FpsBomb => "Bombe",
            Action::FpsLastWeapon => "Dernière arme",
            Action::FpsNextWeapon => "Arme suivante",
            Action::FpsPrevWeapon => "Arme précédente",
            Action::FpsHe => "Grenade HE",
            Action::FpsFlash => "Flash",
            Action::FpsSmoke => "Fumigène",
            Action::FpsDecoy => "Leurre",
            Action::FpsMolotov => "Molotov",
            Action::FpsScoreboard => "Scores",
            Action::FpsChat => "Tchat (tous)",
            Action::FpsTeamChat => "Tchat d'équipe",
            Action::FpsRadio1 => "Radio : ordres",
            Action::FpsRadio2 => "Radio : groupe",
            Action::FpsRadio3 => "Radio : rapports",
            Action::FpsVoice => "Micro",
            Action::FpsSpray => "Graffiti",
            Action::FpsPing => "Signaler (ping)",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Action::Attack => "attack",
            Action::Use => "use",
            Action::Pick => "pick",
            Action::Forward => "forward",
            Action::Back => "back",
            Action::Left => "left",
            Action::Right => "right",
            Action::Jump => "jump",
            Action::Sneak => "sneak",
            Action::Sprint => "sprint",
            Action::Inventory => "inventory",
            Action::Chat => "chat",
            Action::Zoom => "zoom",
            Action::DayNight => "daynight",
            Action::Command => "command",
            Action::Debug => "debug",
            Action::Screenshot => "screenshot",
            Action::FpsForward => "fps_forward",
            Action::FpsBack => "fps_back",
            Action::FpsLeft => "fps_left",
            Action::FpsRight => "fps_right",
            Action::FpsJump => "fps_jump",
            Action::FpsDuck => "fps_duck",
            Action::FpsWalk => "fps_walk",
            Action::FpsFire => "fps_fire",
            Action::FpsScope => "fps_scope",
            Action::FpsReload => "reload",
            Action::FpsUse => "fps_use",
            Action::FpsInspect => "fps_inspect",
            Action::FpsDrop => "fps_drop",
            Action::FpsBuy => "buy",
            Action::FpsAutoBuy => "fps_autobuy",
            Action::FpsRebuy => "fps_rebuy",
            Action::FpsPrimary => "fps_primary",
            Action::FpsSecondary => "fps_secondary",
            Action::FpsKnife => "fps_knife",
            Action::FpsGrenade => "fps_grenade",
            Action::FpsBomb => "fps_bomb",
            Action::FpsLastWeapon => "fps_lastweapon",
            Action::FpsNextWeapon => "fps_nextweapon",
            Action::FpsPrevWeapon => "fps_prevweapon",
            Action::FpsHe => "fps_he",
            Action::FpsFlash => "fps_flash",
            Action::FpsSmoke => "fps_smoke",
            Action::FpsDecoy => "fps_decoy",
            Action::FpsMolotov => "fps_molotov",
            Action::FpsScoreboard => "fps_scoreboard",
            Action::FpsChat => "fps_chat",
            Action::FpsTeamChat => "fps_teamchat",
            Action::FpsRadio1 => "fps_radio1",
            Action::FpsRadio2 => "fps_radio2",
            Action::FpsRadio3 => "fps_radio3",
            Action::FpsVoice => "fps_voice",
            Action::FpsSpray => "fps_spray",
            Action::FpsPing => "fps_ping",
        }
    }

    /// Touche par défaut : celles de Minecraft pour l'onglet Build, celles de CS:GO pour
    /// l'onglet FPS (sans touche quand CS:GO n'en donne pas, ou quand elle est déjà prise
    /// par une action commune : achat auto sur F3 = debug).
    fn default_bind(self) -> Option<Bind> {
        match self {
            Action::Attack => Some(Bind::Mouse(Mouse::Left)),
            Action::Use => Some(Bind::Mouse(Mouse::Right)),
            Action::Pick => Some(Bind::Mouse(Mouse::Middle)),
            Action::Forward => Some(Bind::Key(KeyCode::KeyW)),
            Action::Back => Some(Bind::Key(KeyCode::KeyS)),
            Action::Left => Some(Bind::Key(KeyCode::KeyA)),
            Action::Right => Some(Bind::Key(KeyCode::KeyD)),
            Action::Jump => Some(Bind::Key(KeyCode::Space)),
            Action::Sneak => Some(Bind::Key(KeyCode::ShiftLeft)),
            Action::Sprint => Some(Bind::Key(KeyCode::ControlLeft)),
            Action::Inventory => Some(Bind::Key(KeyCode::KeyE)),
            Action::Chat => Some(Bind::Key(KeyCode::KeyT)),
            Action::Zoom => Some(Bind::Key(KeyCode::KeyC)),
            Action::DayNight => Some(Bind::Key(KeyCode::KeyN)),
            Action::Command => Some(Bind::Key(KeyCode::Slash)),
            Action::Debug => Some(Bind::Key(KeyCode::F3)),
            Action::Screenshot => Some(Bind::Key(KeyCode::F2)),
            Action::FpsForward => Some(Bind::Key(KeyCode::KeyW)),
            Action::FpsBack => Some(Bind::Key(KeyCode::KeyS)),
            Action::FpsLeft => Some(Bind::Key(KeyCode::KeyA)),
            Action::FpsRight => Some(Bind::Key(KeyCode::KeyD)),
            Action::FpsJump => Some(Bind::Key(KeyCode::Space)),
            Action::FpsDuck => Some(Bind::Key(KeyCode::ControlLeft)),
            Action::FpsWalk => Some(Bind::Key(KeyCode::ShiftLeft)),
            Action::FpsFire => Some(Bind::Mouse(Mouse::Left)),
            Action::FpsScope => Some(Bind::Mouse(Mouse::Right)),
            Action::FpsReload => Some(Bind::Key(KeyCode::KeyR)),
            Action::FpsUse => Some(Bind::Key(KeyCode::KeyE)),
            Action::FpsInspect => Some(Bind::Key(KeyCode::KeyF)),
            Action::FpsDrop => Some(Bind::Key(KeyCode::KeyG)),
            Action::FpsBuy => Some(Bind::Key(KeyCode::KeyB)),
            Action::FpsAutoBuy => None,
            Action::FpsRebuy => Some(Bind::Key(KeyCode::F4)),
            Action::FpsPrimary => Some(Bind::Key(KeyCode::Digit1)),
            Action::FpsSecondary => Some(Bind::Key(KeyCode::Digit2)),
            Action::FpsKnife => Some(Bind::Key(KeyCode::Digit3)),
            Action::FpsGrenade => Some(Bind::Key(KeyCode::Digit4)),
            Action::FpsBomb => Some(Bind::Key(KeyCode::Digit5)),
            Action::FpsLastWeapon => Some(Bind::Key(KeyCode::KeyQ)),
            Action::FpsNextWeapon => None,
            Action::FpsPrevWeapon => None,
            Action::FpsHe => None,
            Action::FpsFlash => None,
            Action::FpsSmoke => None,
            Action::FpsDecoy => None,
            Action::FpsMolotov => None,
            Action::FpsScoreboard => Some(Bind::Key(KeyCode::Tab)),
            Action::FpsChat => Some(Bind::Key(KeyCode::KeyY)),
            Action::FpsTeamChat => Some(Bind::Key(KeyCode::KeyU)),
            Action::FpsRadio1 => Some(Bind::Key(KeyCode::KeyZ)),
            Action::FpsRadio2 => Some(Bind::Key(KeyCode::KeyX)),
            Action::FpsRadio3 => Some(Bind::Key(KeyCode::KeyC)),
            Action::FpsVoice => Some(Bind::Key(KeyCode::KeyK)),
            Action::FpsSpray => Some(Bind::Key(KeyCode::KeyT)),
            Action::FpsPing => Some(Bind::Mouse(Mouse::Middle)),
        }
    }
}

/// Touche (ou aucune) de chaque action, et réglages « bascule » du mode FPS.
#[derive(Clone)]
pub struct Bindings {
    keys: [Option<Bind>; Action::ALL.len()],
    /// Mode FPS : un appui sur « Marcher » / « S'accroupir » bascule au lieu de maintenir.
    pub walk_toggle: bool,
    pub duck_toggle: bool,
}

impl Default for Bindings {
    fn default() -> Self {
        Self {
            keys: Action::ALL.map(|a| a.default_bind()),
            walk_toggle: false,
            duck_toggle: false,
        }
    }
}

impl Bindings {
    fn index(a: Action) -> usize {
        Action::ALL.iter().position(|x| *x == a).unwrap_or(0)
    }

    pub fn get(&self, a: Action) -> Option<Bind> {
        self.keys[Self::index(a)]
    }

    /// Assigne `key` à `a` ; l'action du même onglet qui avait déjà cette touche la perd
    /// (une touche peut servir à la fois en Build et en FPS).
    pub fn bind(&mut self, a: Action, key: Bind) {
        for (i, k) in self.keys.iter_mut().enumerate() {
            if *k == Some(key) && Action::ALL[i].group().overlaps(a.group()) {
                *k = None;
            }
        }
        self.keys[Self::index(a)] = Some(key);
    }

    pub fn unbind(&mut self, a: Action) {
        self.keys[Self::index(a)] = None;
    }

    pub fn held(&self, input: &Input, a: Action) -> bool {
        self.get(a).is_some_and(|k| k.held(input))
    }

    pub fn pressed(&self, input: &Input, a: Action) -> bool {
        self.get(a).is_some_and(|k| k.pressed(input))
    }

    pub fn load(path: &Path) -> Self {
        let mut b = Self::default();
        let Ok(text) = fs::read_to_string(path) else {
            return b;
        };
        for line in text.lines() {
            let Some((name, value)) = line.split_once('=') else {
                continue;
            };
            let (name, v) = (name.trim(), value.trim());
            match name {
                "fps_walk_toggle" => b.walk_toggle = v == "1",
                "fps_duck_toggle" => b.duck_toggle = v == "1",
                _ => {}
            }
            let Some(a) = Action::ALL.into_iter().find(|a| a.key() == name) else {
                continue;
            };
            if v.is_empty() || v == "none" {
                b.unbind(a);
            } else if let Some(k) = Bind::from_name(v) {
                b.keys[Self::index(a)] = Some(k);
            }
        }
        b
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let mut s = String::from("# Touches de Minelib (touches physiques, noms QWERTY)\n");
        for a in Action::ALL {
            s += &format!(
                "{}={}\n",
                a.key(),
                self.get(a).map_or("none".into(), |k| k.to_name())
            );
        }
        s += &format!("fps_walk_toggle={}\n", self.walk_toggle as u8);
        s += &format!("fps_duck_toggle={}\n", self.duck_toggle as u8);
        fs::write(path, s)
    }
}

/// Touches qu'on peut assigner (nom = celui de `KeyCode`).
const KEYS: &[KeyCode] = &[
    KeyCode::KeyA,
    KeyCode::KeyB,
    KeyCode::KeyC,
    KeyCode::KeyD,
    KeyCode::KeyE,
    KeyCode::KeyF,
    KeyCode::KeyG,
    KeyCode::KeyH,
    KeyCode::KeyI,
    KeyCode::KeyJ,
    KeyCode::KeyK,
    KeyCode::KeyL,
    KeyCode::KeyM,
    KeyCode::KeyN,
    KeyCode::KeyO,
    KeyCode::KeyP,
    KeyCode::KeyQ,
    KeyCode::KeyR,
    KeyCode::KeyS,
    KeyCode::KeyT,
    KeyCode::KeyU,
    KeyCode::KeyV,
    KeyCode::KeyW,
    KeyCode::KeyX,
    KeyCode::KeyY,
    KeyCode::KeyZ,
    KeyCode::Digit0,
    KeyCode::Digit1,
    KeyCode::Digit2,
    KeyCode::Digit3,
    KeyCode::Digit4,
    KeyCode::Digit5,
    KeyCode::Digit6,
    KeyCode::Digit7,
    KeyCode::Digit8,
    KeyCode::Digit9,
    KeyCode::F1,
    KeyCode::F2,
    KeyCode::F3,
    KeyCode::F4,
    KeyCode::F5,
    KeyCode::F6,
    KeyCode::F7,
    KeyCode::F8,
    KeyCode::F9,
    KeyCode::F10,
    KeyCode::F11,
    KeyCode::F12,
    KeyCode::Space,
    KeyCode::ShiftLeft,
    KeyCode::ShiftRight,
    KeyCode::ControlLeft,
    KeyCode::ControlRight,
    KeyCode::AltLeft,
    KeyCode::AltRight,
    KeyCode::Tab,
    KeyCode::CapsLock,
    KeyCode::Enter,
    KeyCode::Backspace,
    KeyCode::Backquote,
    KeyCode::Minus,
    KeyCode::Equal,
    KeyCode::BracketLeft,
    KeyCode::BracketRight,
    KeyCode::Backslash,
    KeyCode::IntlBackslash,
    KeyCode::Semicolon,
    KeyCode::Quote,
    KeyCode::Comma,
    KeyCode::Period,
    KeyCode::Slash,
    KeyCode::ArrowUp,
    KeyCode::ArrowDown,
    KeyCode::ArrowLeft,
    KeyCode::ArrowRight,
    KeyCode::Insert,
    KeyCode::Delete,
    KeyCode::Home,
    KeyCode::End,
    KeyCode::PageUp,
    KeyCode::PageDown,
    KeyCode::Numpad0,
    KeyCode::Numpad1,
    KeyCode::Numpad2,
    KeyCode::Numpad3,
    KeyCode::Numpad4,
    KeyCode::Numpad5,
    KeyCode::Numpad6,
    KeyCode::Numpad7,
    KeyCode::Numpad8,
    KeyCode::Numpad9,
    KeyCode::NumpadAdd,
    KeyCode::NumpadSubtract,
    KeyCode::NumpadMultiply,
    KeyCode::NumpadDivide,
    KeyCode::NumpadDecimal,
    KeyCode::NumpadEnter,
];

pub fn is_bindable(k: KeyCode) -> bool {
    KEYS.contains(&k)
}

fn key_from_name(name: &str) -> Option<KeyCode> {
    KEYS.iter().copied().find(|k| format!("{k:?}") == name)
}

/// Nom affiché d'une touche : le caractère de la disposition du clavier quand on le connaît.
pub fn key_name(k: KeyCode, input: &Input) -> String {
    let named = match k {
        KeyCode::Space => Some("Espace"),
        KeyCode::ShiftLeft => Some("Maj gauche"),
        KeyCode::ShiftRight => Some("Maj droite"),
        KeyCode::ControlLeft => Some("Ctrl gauche"),
        KeyCode::ControlRight => Some("Ctrl droit"),
        KeyCode::AltLeft => Some("Alt"),
        KeyCode::AltRight => Some("Alt Gr"),
        KeyCode::Tab => Some("Tab"),
        KeyCode::CapsLock => Some("Verr. maj"),
        KeyCode::Enter => Some("Entrée"),
        KeyCode::Backspace => Some("Retour arr."),
        KeyCode::ArrowUp => Some("Haut"),
        KeyCode::ArrowDown => Some("Bas"),
        KeyCode::ArrowLeft => Some("Gauche"),
        KeyCode::ArrowRight => Some("Droite"),
        KeyCode::Insert => Some("Inser"),
        KeyCode::Delete => Some("Suppr"),
        KeyCode::Home => Some("Début"),
        KeyCode::End => Some("Fin"),
        KeyCode::PageUp => Some("Page haut"),
        KeyCode::PageDown => Some("Page bas"),
        KeyCode::NumpadEnter => Some("Entrée (pavé)"),
        _ if minelib_platform::layout_label(k).is_some() || input.key_labels.contains_key(&k) => {
            None
        }
        KeyCode::Slash => Some("/"),
        KeyCode::Comma => Some(","),
        KeyCode::Period => Some("."),
        KeyCode::Semicolon => Some(";"),
        KeyCode::Quote => Some("'"),
        KeyCode::BracketLeft => Some("["),
        KeyCode::BracketRight => Some("]"),
        KeyCode::Backslash => Some("\\"),
        KeyCode::Minus => Some("-"),
        KeyCode::Equal => Some("="),
        KeyCode::Backquote => Some("`"),
        _ => None,
    };
    if let Some(n) = named {
        return n.into();
    }
    if let Some(l) = input
        .key_labels
        .get(&k)
        .cloned()
        .or_else(|| minelib_platform::layout_label(k))
    {
        return l;
    }
    let raw = format!("{k:?}");
    let short = raw
        .strip_prefix("Key")
        .or_else(|| raw.strip_prefix("Digit"))
        .unwrap_or(&raw);
    short.replace("Numpad", "Pavé ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_a_used_key_unbinds_the_previous_action() {
        let mut b = Bindings::default();
        b.bind(Action::Jump, Bind::Key(KeyCode::KeyW));
        assert_eq!(b.get(Action::Jump), Some(Bind::Key(KeyCode::KeyW)));
        b.bind(Action::Pick, Bind::Mouse(Mouse::Left));
        assert_eq!(b.get(Action::Attack), None);
        assert_eq!(b.get(Action::Forward), None);
        b.unbind(Action::Zoom);
        let dir = std::env::temp_dir().join(format!("minelib-keys-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("keys.txt");
        b.save(&path).unwrap();
        let c = Bindings::load(&path);
        assert_eq!(c.get(Action::Jump), Some(Bind::Key(KeyCode::KeyW)));
        assert_eq!(c.get(Action::Pick), Some(Bind::Mouse(Mouse::Left)));
        assert_eq!(c.get(Action::Use), Some(Bind::Mouse(Mouse::Right)));
        assert_eq!(c.get(Action::Forward), None);
        assert_eq!(c.get(Action::Zoom), None);
        assert_eq!(c.get(Action::Sneak), Some(Bind::Key(KeyCode::ShiftLeft)));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn build_and_fps_tabs_keep_their_own_keys() {
        let mut b = Bindings::default();
        // Même touche dans les deux onglets : W avance en Build comme en FPS.
        assert_eq!(b.get(Action::FpsForward), b.get(Action::Forward));
        // Changer une touche FPS ne retire rien en Build…
        b.bind(Action::FpsJump, Bind::Key(KeyCode::KeyE));
        assert_eq!(b.get(Action::Inventory), Some(Bind::Key(KeyCode::KeyE)));
        // … mais l'action FPS qui l'avait la perd, et une action commune aussi.
        assert_eq!(b.get(Action::FpsUse), None);
        b.bind(Action::FpsBuy, Bind::Key(KeyCode::F3));
        assert_eq!(b.get(Action::Debug), None);
        assert!(!Action::tab(false).contains(&Action::FpsBuy));
        assert!(Action::tab(true).contains(&Action::Debug));
        b.walk_toggle = true;
        let dir = std::env::temp_dir().join(format!("minelib-keys2-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("keys.txt");
        b.save(&path).unwrap();
        let c = Bindings::load(&path);
        assert!(c.walk_toggle && !c.duck_toggle);
        assert_eq!(c.get(Action::FpsJump), Some(Bind::Key(KeyCode::KeyE)));
        let _ = fs::remove_dir_all(dir);
    }
}
