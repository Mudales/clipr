//! User-editable keyboard shortcuts, read from `~/.config/clipr/keys.conf`.

use eframe::egui::{Event, InputState, Key, Modifiers};
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Action {
    Paste,
    Copy,
    Type,
    Save,
    Pin,
    Delete,
    SelectAll,
    NextTab,
    PrevTab,
    Close,
    Settings,
    Actions,
    Undo,
}

const ACTIONS: &[(&str, Action)] = &[
    ("paste", Action::Paste),
    ("copy", Action::Copy),
    ("type", Action::Type),
    ("save", Action::Save),
    ("pin", Action::Pin),
    ("delete", Action::Delete),
    ("select_all", Action::SelectAll),
    ("next_tab", Action::NextTab),
    ("prev_tab", Action::PrevTab),
    ("close", Action::Close),
    ("settings", Action::Settings),
    ("actions", Action::Actions),
    ("undo", Action::Undo),
];

/// Written to keys.conf on first run; also the fallback for missing entries.
const DEFAULTS: &str = "\
# clipr keyboard shortcuts — edit and save; they apply the next time the picker opens.
#
# Mod = ⌘ on macOS, Ctrl on Windows/Linux. Other modifiers: Ctrl, Shift, Alt (Opt),
# Cmd (= the Windows key on Windows).
# Several shortcuts per action are separated by commas; leave empty to disable.
# Key names: A-Z, 0-9, Enter, Tab, Space, Escape, Delete, Backspace, Comma,
# ArrowUp/Down/Left/Right, PageUp/PageDown, Home, End, F1-F12.

paste      = Enter, Mod+V
copy       = Mod+C, Shift+Enter
type       = Mod+Z
save       = Mod+S
pin        = Mod+P
# On a Mac keyboard the key labelled \"delete\" is Backspace, which also edits the
# search text, so it needs ⌘; fn+delete is Delete.
delete     = Delete, Mod+Backspace
select_all = Mod+A
next_tab   = Ctrl+Tab
prev_tab   = Ctrl+Shift+Tab
close      = Escape
settings   = Mod+Comma
actions    = Mod+K
undo       = Mod+Shift+Z

# Modifier for pasting item 1-9 directly (e.g. Mod+1).
quick_paste = Mod

# macOS / Windows: the global shortcut that opens clipr (restart clipr after
# changing). Cmd+Shift+V is ⌘⇧V on macOS and Win+Shift+V on Windows.
# On Linux, set the shortcut in your Hyprland config instead.
hotkey = Cmd+Shift+V
";

#[derive(Clone, Copy, PartialEq)]
pub struct Shortcut {
    pub mods: Modifiers,
    pub key: Key,
}

pub struct Keymap {
    bindings: Vec<(Action, Shortcut)>,
    pub quick: Modifiers,
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    pub hotkey: String,
    /// Problems found in keys.conf, shown in the picker's status line.
    pub errors: Vec<String>,
    /// Actions set to nothing in the file (`pin =`), i.e. disabled.
    empty: Vec<Action>,
    /// Effective setting text by name (`"paste"` → `"Enter, Mod+V"`), for the
    /// Settings page.
    raw: Vec<(String, String)>,
}

/// Editable entries in the order the Settings page shows them, with labels.
pub const EDITABLE: &[(&str, &str)] = &[
    ("paste", "Paste"),
    ("type", "Type out"),
    ("copy", "Copy"),
    ("save", "Save / unsave"),
    ("pin", "Pin / unpin"),
    ("delete", "Delete"),
    ("select_all", "Select all"),
    ("next_tab", "Next tab"),
    ("prev_tab", "Previous tab"),
    ("close", "Close"),
    ("settings", "Settings"),
    ("actions", "Actions menu"),
    ("undo", "Undo delete"),
    ("quick_paste", "Paste 1–9 modifier"),
    ("hotkey", "Open clipr (global, needs restart)"),
];

/// Checks one setting's text; `Err` has a message for the user.
pub fn validate(name: &str, value: &str) -> Result<(), String> {
    match name {
        "hotkey" => {
            if value.is_empty() || parse_shortcut(value).is_ok() {
                Ok(())
            } else {
                Err("e.g. Cmd+Shift+V".into())
            }
        }
        "quick_paste" => value
            .split('+')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .try_for_each(|t| parse_mods(t).map(|_| ()).ok_or_else(|| format!("unknown modifier '{t}'"))),
        _ => value
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .try_for_each(|s| parse_shortcut(s).map(|_| ())),
    }
}

/// Writes keys.conf: the commented defaults with `values` filled in.
pub fn save(values: &[(String, String)]) -> std::io::Result<()> {
    let mut out = String::new();
    for line in DEFAULTS.lines() {
        let name = line.split_once('=').map(|(n, _)| n.trim()).filter(|_| !line.starts_with('#'));
        match name.and_then(|n| values.iter().find(|(k, _)| k == n)) {
            Some((k, v)) => out.push_str(&format!("{k:<10} = {v}\n")),
            None => {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    let path = config_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, out)
}

pub fn config_path() -> PathBuf {
    // Windows: %APPDATA%\clipr, next to the history database.
    #[cfg(windows)]
    return crate::db::data_dir().join("keys.conf");
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".config"));
    #[cfg(not(windows))]
    base.join("clipr").join("keys.conf")
}

/// Creates keys.conf with the defaults if it doesn't exist yet.
pub fn ensure_config() -> PathBuf {
    let path = config_path();
    if !path.exists() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&path, DEFAULTS);
    }
    path
}

fn parse_mods(token: &str) -> Option<Modifiers> {
    Some(match token.to_ascii_lowercase().as_str() {
        "mod" | "cmd" | "command" | "super" | "cmdorctrl" => Modifiers::COMMAND,
        "ctrl" | "control" => Modifiers::CTRL,
        "shift" => Modifiers::SHIFT,
        "alt" | "opt" | "option" => Modifiers::ALT,
        _ => return None,
    })
}

fn parse_key(token: &str) -> Option<Key> {
    let name = match token.to_ascii_lowercase().as_str() {
        "esc" => "Escape",
        "return" => "Enter",
        "del" => "Delete",
        "down" => "ArrowDown",
        "up" => "ArrowUp",
        "left" => "ArrowLeft",
        "right" => "ArrowRight",
        "," => "Comma",
        _ => token,
    };
    Key::from_name(name).or_else(|| {
        // Accept any capitalisation: "enter", "pageup", "a".
        let lower = name.to_ascii_lowercase();
        Key::ALL.iter().copied().find(|k| k.name().to_ascii_lowercase() == lower)
    })
}

fn parse_shortcut(s: &str) -> Result<Shortcut, String> {
    let parts: Vec<&str> = s.split('+').map(str::trim).collect();
    let (key, mods) = parts.split_last().ok_or("empty shortcut")?;
    let mut all = Modifiers::NONE;
    for m in mods {
        all = all | parse_mods(m).ok_or_else(|| format!("unknown modifier '{m}'"))?;
    }
    let key = parse_key(key).ok_or_else(|| format!("unknown key '{key}'"))?;
    Ok(Shortcut { mods: all, key })
}

impl Keymap {
    /// Defaults, overridden by whatever keys.conf sets.
    pub fn load() -> Self {
        let mut map = Self::parse(DEFAULTS);
        let path = ensure_config();
        if let Ok(text) = std::fs::read_to_string(&path) {
            let user = Self::parse(&text);
            let set: Vec<Action> = user.bindings.iter().map(|b| b.0).chain(user.cleared()).collect();
            map.bindings.retain(|(a, _)| !set.contains(a));
            map.bindings.extend(user.bindings);
            if text.lines().any(|l| l.trim_start().starts_with("quick_paste")) {
                map.quick = user.quick;
            }
            if text.lines().any(|l| l.trim_start().starts_with("hotkey")) {
                map.hotkey = user.hotkey;
            }
            map.errors = user.errors;
            for (k, v) in user.raw {
                match map.raw.iter_mut().find(|(n, _)| *n == k) {
                    Some(entry) => entry.1 = v,
                    None => map.raw.push((k, v)),
                }
            }
        }
        map
    }

    /// The setting's current text, e.g. `"Enter, Mod+V"`.
    pub fn raw(&self, name: &str) -> String {
        self.raw.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone()).unwrap_or_default()
    }

    /// Actions the user explicitly set to nothing (`pin =`).
    fn cleared(&self) -> Vec<Action> {
        self.empty.clone()
    }

    fn parse(text: &str) -> Self {
        let mut map = Keymap {
            bindings: Vec::new(),
            quick: Modifiers::COMMAND,
            hotkey: "Cmd+Shift+V".into(),
            errors: Vec::new(),
            empty: Vec::new(),
            raw: Vec::new(),
        };
        for (n, line) in text.lines().enumerate() {
            let line = line.split('#').next().unwrap_or("").trim();
            let Some((name, value)) = line.split_once('=') else { continue };
            let (name, value) = (name.trim(), value.trim());
            let err = |e: String| format!("keys.conf line {}: {e}", n + 1);
            if name == "hotkey" || name == "quick_paste" || ACTIONS.iter().any(|(a, _)| *a == name) {
                map.raw.push((name.to_owned(), value.to_owned()));
            }
            match name {
                "hotkey" => map.hotkey = value.to_owned(),
                "quick_paste" => {
                    let mut m = Modifiers::NONE;
                    for t in value.split('+').map(str::trim).filter(|t| !t.is_empty()) {
                        match parse_mods(t) {
                            Some(x) => m = m | x,
                            None => map.errors.push(err(format!("unknown modifier '{t}'"))),
                        }
                    }
                    map.quick = m;
                }
                _ => {
                    let Some(&(_, action)) = ACTIONS.iter().find(|(a, _)| *a == name) else {
                        map.errors.push(err(format!("unknown action '{name}'")));
                        continue;
                    };
                    let shortcuts: Vec<&str> =
                        value.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
                    if shortcuts.is_empty() {
                        map.empty.push(action);
                    }
                    for s in shortcuts {
                        match parse_shortcut(s) {
                            Ok(sc) => map.bindings.push((action, sc)),
                            Err(e) => map.errors.push(err(e)),
                        }
                    }
                }
            }
        }
        map
    }

    /// Takes this frame's shortcut presses out of the input (so the search
    /// field never sees them) and returns the matching actions.
    pub fn take_actions(&self, input: &mut InputState) -> Vec<Action> {
        let mut actions = Vec::new();
        input.events.retain(|event| {
            // egui turns ⌘/Ctrl+C/X/V into Copy/Cut/Paste events.
            let (mods, key, repeat) = match event {
                Event::Key { key, modifiers, pressed: true, repeat, .. } => (*modifiers, *key, *repeat),
                Event::Copy => (Modifiers::COMMAND, Key::C, false),
                Event::Cut => (Modifiers::COMMAND, Key::X, false),
                Event::Paste(_) => (Modifiers::COMMAND, Key::V, false),
                _ => return true,
            };
            let hit = self
                .bindings
                .iter()
                .find(|(_, s)| s.key == key && mods.matches_exact(s.mods));
            match hit {
                Some((action, _)) => {
                    if !repeat {
                        actions.push(*action);
                    }
                    false
                }
                None => true,
            }
        });
        actions
    }

    /// First shortcut of `action`, formatted for the footer (e.g. "⌘Z").
    pub fn label(&self, action: Action) -> Option<String> {
        let mut shortcuts = self.bindings.iter().filter(|(a, _)| *a == action).map(|(_, s)| s);
        let first = shortcuts.clone().next()?;
        // Mac keyboards have no Delete key (their "delete" is Backspace), so
        // show another shortcut there if there is one.
        let best = if cfg!(target_os = "macos") {
            shortcuts.find(|s| s.key != Key::Delete).unwrap_or(first)
        } else {
            first
        };
        Some(format_shortcut(best))
    }
}

pub fn format_mods(m: Modifiers) -> String {
    let mac = cfg!(target_os = "macos");
    let mut out = String::new();
    if m.ctrl {
        out.push_str("Ctrl+");
    }
    if m.alt {
        out.push_str(if mac { "Opt+" } else { "Alt+" });
    }
    if m.shift {
        out.push_str(if mac { "⇧" } else { "Shift+" });
    }
    if m.command || m.mac_cmd {
        out.push_str(if mac { "⌘" } else { "Ctrl+" });
    }
    out
}

fn format_shortcut(s: &Shortcut) -> String {
    let mac = cfg!(target_os = "macos");
    let key = match s.key {
        Key::Enter if mac => "↩".to_owned(),
        Key::Delete => "Del".to_owned(),
        Key::Backspace if mac => "⌫".to_owned(),
        Key::Backspace => "Backspace".to_owned(),
        Key::Escape => "Esc".to_owned(),
        Key::Comma => ",".to_owned(),
        k => k.symbol_or_name().to_owned(),
    };
    format!("{}{key}", format_mods(s.mods))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_shortcuts() {
        let s = parse_shortcut("Ctrl+Shift+Tab").unwrap();
        assert_eq!(s.key, Key::Tab);
        assert!(s.mods.ctrl && s.mods.shift);
        assert_eq!(parse_shortcut("mod+z").unwrap().mods, Modifiers::COMMAND);
        assert_eq!(parse_shortcut("Del").unwrap().key, Key::Delete);
        assert!(parse_shortcut("Mod+Nope").is_err());
    }

    #[test]
    fn defaults_parse_cleanly() {
        let map = Keymap::parse(DEFAULTS);
        assert!(map.errors.is_empty(), "{:?}", map.errors);
        assert!(map.label(Action::Type).is_some());
    }

    #[test]
    fn user_file_overrides_and_clears() {
        let user = Keymap::parse("type = Mod+T\npin =\n");
        assert_eq!(user.bindings.len(), 1);
        assert_eq!(user.cleared(), vec![Action::Pin]);
    }
}
