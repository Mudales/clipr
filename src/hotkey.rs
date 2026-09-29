//! The global "open clipr" shortcut (macOS and Windows; on Linux the
//! compositor binds `clipr toggle` instead).

use anyhow::Result;
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

/// Registers the global shortcut from keys.conf (default Cmd+Shift+V). Must be
/// called on the main thread before the event loop starts; keep the returned
/// manager alive.
pub fn register_hotkey(spec: &str, on_hotkey: impl Fn() + Send + Sync + 'static) -> Result<GlobalHotKeyManager> {
    let manager = GlobalHotKeyManager::new()?;
    // "Mod" means ⌘ on macOS and Ctrl on Windows, like in the picker.
    let m = if cfg!(target_os = "macos") { "Cmd" } else { "Ctrl" };
    let normalized = spec.replace("Mod", m).replace("mod", m);
    let hotkey = match normalized.parse::<HotKey>() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("clipr: bad hotkey {spec:?} in keys.conf ({e}), using Cmd/Win+Shift+V");
            HotKey::new(Some(Modifiers::SUPER | Modifiers::SHIFT), Code::KeyV)
        }
    };
    manager.register(hotkey)?;
    GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
        if event.state == HotKeyState::Pressed {
            on_hotkey();
        }
    }));
    eprintln!("clipr: hotkey {spec}");
    Ok(manager)
}
