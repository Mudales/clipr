//! The background process: records clipboard history, owns the clipboard when
//! pasting, launches the picker popup and sends the paste keystroke.

use crate::db::{Db, Payload};
use crate::settings::Settings;
use crate::{ipc, platform};
use anyhow::{Context, Result, bail};
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::process::Child;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(250);

struct Popup {
    child: Option<Child>,
    /// App that was focused before the popup opened, so we can paste into it.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    prev_app: Option<i32>,
}

static POPUP: Mutex<Popup> = Mutex::new(Popup { child: None, prev_app: None });

pub fn run() -> Result<()> {
    if ipc::daemon_running() {
        bail!("clipr is already running");
    }
    // Create the schema once up front so the threads below don't race on it.
    drop(Db::open()?);
    let path = ipc::socket_path();
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).with_context(|| format!("binding {}", path.display()))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    eprintln!("clipr: listening on {}", path.display());

    thread::spawn(|| log_err("watcher", watch_clipboard()));
    thread::spawn(move || log_err("server", serve(listener)));

    // On macOS the picker lives in this process (hidden until the hotkey).
    #[cfg(target_os = "macos")]
    {
        platform::accessibility_trusted(true); // ask once, up front
        let _hotkey = platform::register_hotkey(&crate::keys::Keymap::load().hotkey, toggle_popup)?;
        return crate::ui::run(crate::ui::Mode::Resident);
    }

    #[cfg(not(target_os = "macos"))]
    loop {
        thread::park();
    }
}

fn log_err(what: &str, r: Result<()>) {
    if let Err(e) = r {
        eprintln!("clipr {what}: {e:#}");
    }
}

#[cfg(target_os = "macos")]
fn watch_clipboard() -> Result<()> {
    let mut db = Db::open()?;
    let mut clipboard = arboard::Clipboard::new()?;
    let mut last_count = platform::change_count() - 1;

    loop {
        thread::sleep(POLL);
        // macOS exposes a change counter, so we only read the contents when
        // something was actually copied (and can see re-copies of the same text).
        let count = platform::change_count();
        if count == last_count {
            continue;
        }
        last_count = count;
        if platform::should_skip_current() {
            continue;
        }
        let settings = Settings::load();
        if platform::frontmost_app_names().iter().any(|a| settings.ignores_app(a)) {
            continue;
        }
        db.set_limits(settings.history_size, settings.image_limit);
        if let Ok(text) = clipboard.get_text() {
            if !settings.ignores_text(&text) {
                log_err("db", db.add(&text));
            }
        } else if settings.save_images {
            if let Ok(img) = clipboard.get_image() {
                let stored = crate::images::from_rgba(img.width as u32, img.height as u32, img.bytes.into_owned());
                log_err("image", stored.and_then(|s| db.add_image(&s)));
            }
        }
    }
}

/// On Wayland, `wl-paste --watch` runs `clipr store` on every copy — no
/// polling, and it flags password-manager copies as sensitive. Falls back to
/// polling text (X11, or no wl-clipboard installed).
#[cfg(not(target_os = "macos"))]
fn watch_clipboard() -> Result<()> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() && platform::spawn_wl_watchers() {
        loop {
            thread::park();
        }
    }
    let mut db = Db::open()?;
    let mut clipboard = arboard::Clipboard::new()?;
    let mut last: Option<String> = None;
    loop {
        thread::sleep(POLL);
        if let Ok(text) = clipboard.get_text() {
            if last.as_deref() != Some(text.as_str()) {
                let settings = Settings::load();
                db.set_limits(settings.history_size, settings.image_limit);
                if !settings.ignores_text(&text) {
                    log_err("db", db.add(&text));
                }
                last = Some(text);
            }
        }
    }
}

/// `clipr store`: saves the clipboard contents given on stdin (run by `wl-paste --watch`).
pub fn store_from_stdin() -> Result<()> {
    use std::io::Read;
    // wl-paste sets this; "sensitive" = marked secret by a password manager.
    if matches!(std::env::var("CLIPBOARD_STATE").as_deref(), Ok("sensitive" | "clear")) {
        return Ok(());
    }
    let settings = Settings::load();
    #[cfg(target_os = "linux")]
    if !settings.ignore_apps.is_empty()
        && platform::active_window_class().is_some_and(|c| settings.ignores_app(&c))
    {
        return Ok(());
    }
    let mut data = Vec::new();
    std::io::stdin().take((crate::images::MAX_IMAGE_BYTES + 1) as u64).read_to_end(&mut data)?;
    let mut db = Db::open()?;
    db.set_limits(settings.history_size, settings.image_limit);
    if crate::images::looks_like_image(&data) {
        if !settings.save_images {
            return Ok(());
        }
        db.add_image(&crate::images::from_encoded(&data)?)
    } else if let Ok(text) = String::from_utf8(data) {
        if settings.ignores_text(&text) {
            return Ok(());
        }
        db.add(&text)
    } else {
        Ok(())
    }
}

fn serve(listener: UnixListener) -> Result<()> {
    let db = Db::open()?;
    let mut clipboard = arboard::Clipboard::new()?;
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let mut line = String::new();
        if BufReader::new(stream).read_line(&mut line).is_err() {
            continue;
        }
        log_err("command", handle(line.trim(), &db, &mut clipboard));
    }
    Ok(())
}

fn handle(line: &str, db: &Db, clipboard: &mut arboard::Clipboard) -> Result<()> {
    if line.is_empty() {
        return Ok(()); // liveness probe from `ipc::daemon_running`
    }
    let (cmd, arg) = line.split_once(' ').unwrap_or((line, ""));
    if cmd == "TOGGLE" {
        toggle_popup();
        return Ok(());
    }
    // One id, or several ("3,7,9") which are joined line by line.
    let ids: Vec<i64> = arg
        .split(',')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .with_context(|| format!("bad command {line:?}"))?;
    let payload = if let [id] = ids[..] {
        let Some(payload) = db.payload(id)? else { return Ok(()) };
        db.touch(id)?; // move to top of history
        payload
    } else {
        let mut texts = Vec::new();
        for id in &ids {
            if let Some(Payload::Text(t)) = db.payload(*id)? {
                texts.push(t.trim_end_matches('\n').to_owned());
            }
        }
        Payload::Text(texts.join("\n"))
    };

    match (cmd, payload) {
        ("TYPE", Payload::Text(text)) => {
            wait_for_popup_exit();
            platform::type_text(&text)?;
        }
        (cmd @ ("COPY" | "PASTE" | "TYPE"), payload) => {
            match payload {
                Payload::Text(text) => clipboard.set_text(&text)?,
                Payload::Image(png) => clipboard.set_image(crate::images::decode(&png)?)?,
            }
            wait_for_popup_exit();
            // Images can't be typed, so "type" pastes them.
            if cmd != "COPY" {
                platform::send_paste()?;
            }
        }
        _ => bail!("unknown command {cmd:?}"),
    }
    Ok(())
}

/// Opens the picker, or closes it if it is already open.
#[cfg(target_os = "macos")]
fn toggle_popup() {
    let mut popup = POPUP.lock().unwrap();
    let front = platform::frontmost_pid();
    if front.is_some() && front != Some(std::process::id() as i32) {
        popup.prev_app = front;
    }
    crate::ui::request_toggle();
}

/// Gives focus back to the app that was active before the picker opened.
pub fn restore_focus() {
    #[cfg(target_os = "macos")]
    if let Some(pid) = POPUP.lock().unwrap().prev_app {
        platform::activate(pid);
    }
}

/// Opens the picker, or closes it if it is already open.
#[cfg(not(target_os = "macos"))]
fn toggle_popup() {
    let mut popup = POPUP.lock().unwrap();
    if let Some(child) = popup.child.as_mut() {
        if matches!(child.try_wait(), Ok(None)) {
            let _ = child.kill();
            let _ = child.wait();
            popup.child = None;
            return;
        }
    }
    let exe = std::env::current_exe().unwrap_or_else(|_| "clipr".into());
    match std::process::Command::new(exe).arg("pick").spawn() {
        Ok(child) => popup.child = Some(child),
        Err(e) => eprintln!("clipr: cannot open picker: {e}"),
    }
}

/// Waits for the picker to close and focus to return to the previous app, so
/// the keystroke lands in the right window.
fn wait_for_popup_exit() {
    let mut popup = POPUP.lock().unwrap();
    if let Some(mut child) = popup.child.take() {
        let deadline = Instant::now() + Duration::from_millis(1500);
        while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
    drop(popup);
    restore_focus();
    thread::sleep(Duration::from_millis(120));
}
