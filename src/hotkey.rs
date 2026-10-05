//! The global "open clipr" shortcut (macOS and Windows; on Linux the
//! compositor binds `clipr toggle` instead).

use anyhow::Result;
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

/// Registers the global shortcut from keys.conf (default Cmd/Ctrl+Shift+V). Must be
/// called on the main thread before the event loop starts; keep the returned
/// manager alive.
pub fn register_hotkey(spec: &str, on_hotkey: impl Fn() + Send + Sync + 'static) -> Result<GlobalHotKeyManager> {
    let manager = GlobalHotKeyManager::new()?;
    let default = HotKey::new(Some(primary() | Modifiers::SHIFT), Code::KeyV);
    let hotkey = match normalize(spec).parse::<HotKey>() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("clipr: bad hotkey {spec:?} in keys.conf ({e}), using the default");
            default
        }
    };
    // A shortcut another app (or Windows itself) holds must not stop clipr:
    // fall back to the default, and failing that run without one.
    match manager.register(hotkey) {
        Ok(()) => eprintln!("clipr: hotkey {spec}"),
        Err(e) => {
            eprintln!("clipr: can't use hotkey {spec:?} ({e})");
            if hotkey != default && manager.register(default).is_ok() {
                eprintln!("clipr: using the default hotkey instead");
            } else {
                eprintln!("clipr: running without a global hotkey; set another one in keys.conf");
            }
        }
    }
    GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
        if event.state == HotKeyState::Pressed {
            on_hotkey();
        }
    }));
    Ok(manager)
}

/// The hotkey as it should be shown (e.g. in the tray menu).
pub fn display(spec: &str) -> String {
    normalize(spec)
}

/// ⌘ on macOS, Ctrl on Windows.
fn primary() -> Modifiers {
    if cfg!(target_os = "macos") { Modifiers::SUPER } else { Modifiers::CONTROL }
}

/// "Mod" means ⌘ on macOS and Ctrl on Windows, like in the picker. Windows
/// has no ⌘ key, so "Cmd" (the shared default) means Ctrl there too; the
/// Windows key is "Win"/"Super".
fn normalize(spec: &str) -> String {
    let m = if cfg!(target_os = "macos") { "Cmd" } else { "Ctrl" };
    spec.split('+')
        .map(|part| match part.trim() {
            p if p.eq_ignore_ascii_case("mod") => m.to_owned(),
            p if !cfg!(target_os = "macos") && (p.eq_ignore_ascii_case("cmd") || p.eq_ignore_ascii_case("command")) => "Ctrl".to_owned(),
            p => p.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("+")
}
