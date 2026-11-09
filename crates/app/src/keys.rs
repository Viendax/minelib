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
    Command,
    Zoom,
    DayNight,
    Lod,
    Debug,
    Screenshot,
    Buy,
    Reload,
}

impl Action {
    pub const ALL: [Action; 20] = [
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
        Action::Command,
        Action::Zoom,
        Action::DayNight,
        Action::Lod,
        Action::Debug,
        Action::Screenshot,
        Action::Buy,
        Action::Reload,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Action::Attack => "Casser",
            Action::Use => "Poser",
            Action::Pick => "Copier / ping",
            Action::Forward => "Avancer",
            Action::Back => "Reculer",
            Action::Left => "Gauche",
            Action::Right => "Droite",
            Action::Jump => "Sauter / monter",
            Action::Sneak => "S'accroupir",
            Action::Sprint => "Courir",
            Action::Inventory => "Inventaire",
            Action::Chat => "Tchat",
            Action::Command => "Commande",
            Action::Zoom => "Zoom",
            Action::DayNight => "Jour / nuit",
            Action::Lod => "Terrain loin",
            Action::Debug => "Debug (F3)",
            Action::Screenshot => "Capture d'écran",
            Action::Buy => "Menu d'achat (FPS)",
            Action::Reload => "Recharger (FPS)",
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
            Action::Command => "command",
            Action::Zoom => "zoom",
            Action::DayNight => "daynight",
            Action::Lod => "lod",
            Action::Debug => "debug",
            Action::Screenshot => "screenshot",
            Action::Buy => "buy",
            Action::Reload => "reload",
        }
    }

    fn default_bind(self) -> Bind {
        match self {
            Action::Attack => Bind::Mouse(Mouse::Left),
            Action::Use => Bind::Mouse(Mouse::Right),
            Action::Pick => Bind::Mouse(Mouse::Middle),
            other => Bind::Key(other.default_key()),
        }
    }

    fn default_key(self) -> KeyCode {
        match self {
            Action::Attack | Action::Use | Action::Pick => KeyCode::F12,
            Action::Forward => KeyCode::KeyW,
            Action::Back => KeyCode::KeyS,
            Action::Left => KeyCode::KeyA,
            Action::Right => KeyCode::KeyD,
            Action::Jump => KeyCode::Space,
            Action::Sneak => KeyCode::ShiftLeft,
            Action::Sprint => KeyCode::ControlLeft,
            Action::Inventory => KeyCode::KeyE,
            Action::Chat => KeyCode::KeyT,
            // Touche « / » du QWERTY = « ! » en AZERTY : on garde aussi l'ouverture par le
            // caractère « / » tapé (voir le tchat).
            Action::Command => KeyCode::Slash,
            Action::Zoom => KeyCode::KeyC,
            Action::DayNight => KeyCode::KeyN,
            Action::Lod => KeyCode::KeyL,
            Action::Debug => KeyCode::F3,
            Action::Screenshot => KeyCode::F2,
            Action::Buy => KeyCode::KeyB,
            Action::Reload => KeyCode::KeyR,
        }
    }
}

/// Touche (ou aucune) de chaque action.
#[derive(Clone)]
pub struct Bindings {
    keys: [Option<Bind>; Action::ALL.len()],
}

impl Default for Bindings {
    fn default() -> Self {
        Self {
            keys: Action::ALL.map(|a| Some(a.default_bind())),
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

    /// Assigne `key` à `a` ; l'action qui avait déjà cette touche la perd.
    pub fn bind(&mut self, a: Action, key: Bind) {
        for k in &mut self.keys {
            if *k == Some(key) {
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
            let Some(a) = Action::ALL.into_iter().find(|a| a.key() == name.trim()) else {
                continue;
            };
            let v = value.trim();
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
}
