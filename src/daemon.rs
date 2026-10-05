//! The background process: records clipboard history, owns the clipboard when
//! pasting, launches the picker popup and sends the paste keystroke.

use crate::db::{Db, Payload};
use crate::settings::Settings;
use crate::{ipc, platform};
use anyhow::{Context, Result, bail};
use std::process::Child;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(250);

struct Popup {
    child: Option<Child>,
    /// App (macOS pid / Windows window) focused before the popup opened, so
    /// we can paste into it.
    #[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
    prev_app: Option<isize>,
}

static POPUP: Mutex<Popup> = Mutex::new(Popup { child: None, prev_app: None });

pub fn run() -> Result<()> {
    if ipc::daemon_running() {
        bail!("clipr is already running");
    }
    // Create the schema once up front so the threads below don't race on it.
    let db = Db::open()?;
    if crate::boot::is_new_boot() && Settings::load().clear_on_restart {
        match db.clear(false) {
            Ok(n) => eprintln!("clipr: cleared {n} clips after restart (kept pinned & saved)"),
            Err(e) => eprintln!("clipr: couldn't clear after restart: {e}"),
        }
    }
    drop(db);
    let listener = ipc::listen()?;

    thread::spawn(|| log_err("watcher", watch_clipboard()));
    thread::spawn(move || log_err("server", serve(listener)));

    // On macOS and Windows the picker lives in this process (hidden until the
    // hotkey): a freshly started process isn't allowed to take focus there.
    #[cfg(any(target_os = "macos", windows))]
    {
        #[cfg(target_os = "macos")]
        platform::accessibility_trusted(true); // ask once, up front
        let _hotkey = crate::hotkey::register_hotkey(&crate::keys::Keymap::load().hotkey, toggle_popup)?;
        // Just updated: open the picker once, showing "Updated to …".
        if crate::update::just_updated() {
            toggle_popup();
        }
        return crate::ui::run(crate::ui::Mode::Resident);
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    {
        if crate::update::just_updated() {
            thread::sleep(Duration::from_millis(500));
            toggle_popup();
        }
        // The tray icon follows the setting (changed from the picker process).
        loop {
            crate::tray::sync(Settings::load().show_tray, "");
            thread::sleep(Duration::from_secs(2));
        }
    }
}

fn log_err(what: &str, r: Result<()>) {
    if let Err(e) = r {
        eprintln!("clipr {what}: {e:#}");
    }
}

/// macOS and Windows expose a cheap change counter, so the clipboard is only
/// read when something was actually copied (and re-copies are seen too).
#[cfg(any(target_os = "macos", windows))]
fn watch_clipboard() -> Result<()> {
    let mut db = Db::open()?;
    let mut clipboard = arboard::Clipboard::new()?;
    let mut last_count = platform::change_count() - 1;

    loop {
        thread::sleep(POLL);
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
#[cfg(not(any(target_os = "macos", windows)))]
fn watch_clipboard() -> Result<()> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() && platform::spawn_wl_watchers() {
        // wl-paste --watch records copies, but on Hyprland it never reports
        // the clipboard being emptied, so watch for that here (cheap: only
        // asks which formats are offered).
        let db = Db::open()?;
        // Start from the current state, so an empty clipboard at login isn't "restored".
        let mut had_data = !platform::clipboard_is_empty();
        loop {
            thread::sleep(Duration::from_millis(400));
            let empty = platform::clipboard_is_empty();
            if empty && had_data && Settings::load().keep_clipboard {
                log_err("keep clipboard", platform::restore_clipboard(&db));
            }
            had_data = !empty;
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
    let settings = Settings::load();
    // wl-paste sets this. "sensitive" = marked secret by a password manager;
    // "clear" = emptied on purpose (e.g. a password manager after 30s): leave
    // it. "nil" = emptied because the app that owned it closed: put it back.
    #[cfg(target_os = "linux")]
    let mark = |ok: bool| platform::mark_restorable(ok);
    #[cfg(not(target_os = "linux"))]
    let mark = |_: bool| ();
    match std::env::var("CLIPBOARD_STATE").as_deref() {
        Ok("sensitive") => {
            mark(false); // a password: never put it back
            return Ok(());
        }
        Ok("clear") => return Ok(()),
        #[cfg(target_os = "linux")]
        Ok("nil") => {
            if settings.keep_clipboard {
                return platform::restore_clipboard(&Db::open()?);
            }
            return Ok(());
        }
        _ => {}
    }
    #[cfg(target_os = "linux")]
    if !settings.ignore_apps.is_empty()
        && platform::active_window_class().is_some_and(|c| settings.ignores_app(&c))
    {
        mark(false);
        return Ok(());
    }
    let mut data = Vec::new();
    std::io::stdin().take((crate::images::MAX_IMAGE_BYTES + 1) as u64).read_to_end(&mut data)?;
    let mut db = Db::open()?;
    db.set_limits(settings.history_size, settings.image_limit);
    if crate::images::looks_like_image(&data) {
        if !settings.save_images {
            mark(false);
            return Ok(());
        }
        db.add_image(&crate::images::from_encoded(&data)?)?;
        mark(true);
        Ok(())
    } else if let Ok(text) = String::from_utf8(data) {
        if settings.ignores_text(&text) {
            mark(false);
            return Ok(());
        }
        db.add(&text)?;
        mark(!text.trim().is_empty());
        Ok(())
    } else {
        Ok(())
    }
}

fn serve(listener: ipc::Listener) -> Result<()> {
    let db = Db::open()?;
    let mut clipboard = arboard::Clipboard::new()?;
    listener.serve(|line| log_err("command", handle(line, &db, &mut clipboard)));
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
    // ACTION <n> <ids> [paste|type|copy]: run action n on the clip(s), then
    // paste / type / copy the result (default: what the action says).
    let (action, arg) = match cmd {
        "ACTION" => {
            let mut parts = arg.split(' ');
            let n: usize = parts.next().unwrap_or("").parse().with_context(|| format!("bad command {line:?}"))?;
            let ids = parts.next().unwrap_or("");
            let settings = Settings::load();
            let mut action = settings.actions.get(n).cloned().with_context(|| format!("no action #{n}"))?;
            if let Some(then) = parts.next().and_then(crate::actions::Then::parse) {
                if action.then != crate::actions::Then::Run {
                    action.then = then;
                }
            }
            (Some(action), ids)
        }
        _ => (None, arg),
    };
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

    if let Some(action) = action {
        let Payload::Text(text) = payload else { bail!("actions work on text clips only") };
        let output = action.run(&text);
        wait_for_popup_exit();
        match (output?, action.then) {
            (Some(out), crate::actions::Then::Paste) => {
                clipboard.set_text(&out)?;
                platform::send_paste()?;
            }
            (Some(out), crate::actions::Then::Type) => platform::type_text(&out)?,
            (Some(out), _) => clipboard.set_text(&out)?,
            (None, _) => {}
        }
        return Ok(());
    }

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
#[cfg(any(target_os = "macos", windows))]
fn toggle_popup() {
    let mut popup = POPUP.lock().unwrap();
    if let Some(front) = platform::frontmost_app() {
        if !platform::is_own(front) {
            popup.prev_app = Some(front);
        }
    }
    crate::ui::request_toggle();
}

/// Opens the picker (from the tray icon), on the Settings page if asked.
#[cfg(any(target_os = "macos", windows))]
pub fn open_popup(settings: bool) {
    let mut popup = POPUP.lock().unwrap();
    if let Some(front) = platform::frontmost_app() {
        // Clicking the tray focuses the taskbar; don't paste into that.
        #[cfg(windows)]
        let skip = platform::is_shell(front);
        #[cfg(not(windows))]
        let skip = false;
        if !platform::is_own(front) && !skip {
            popup.prev_app = Some(front);
        }
    }
    crate::ui::request_open(settings);
}

/// Gives focus back to the app that was active before the picker opened.
pub fn restore_focus() {
    #[cfg(any(target_os = "macos", windows))]
    if let Some(pid) = POPUP.lock().unwrap().prev_app {
        platform::activate(pid);
    }
}

/// Opens the picker, or closes it if it is already open.
#[cfg(not(any(target_os = "macos", windows)))]
fn toggle_popup() {
    let mut popup = POPUP.lock().unwrap();
    if close_picker(&mut popup) {
        return;
    }
    spawn_picker(&mut popup, false);
}

/// Opens the picker (from the tray icon), on the Settings page if asked.
#[cfg(not(any(target_os = "macos", windows)))]
pub fn open_popup(settings: bool) {
    let mut popup = POPUP.lock().unwrap();
    close_picker(&mut popup); // if open, reopen fresh so it comes to the front
    spawn_picker(&mut popup, settings);
}

/// Tray → Quit: stop the picker and the daemon (the wl-paste watchers die
/// with us).
#[cfg(not(any(target_os = "macos", windows)))]
pub fn quit() {
    close_picker(&mut POPUP.lock().unwrap());
    std::process::exit(0);
}

/// Closes a running picker; returns whether one was running.
#[cfg(not(any(target_os = "macos", windows)))]
fn close_picker(popup: &mut Popup) -> bool {
    if let Some(mut child) = popup.child.take() {
        if matches!(child.try_wait(), Ok(None)) {
            let _ = child.kill();
            let _ = child.wait();
            return true;
        }
    }
    false
}

#[cfg(not(any(target_os = "macos", windows)))]
fn spawn_picker(popup: &mut Popup, settings: bool) {
    let exe = std::env::current_exe().unwrap_or_else(|_| "clipr".into());
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("pick");
    if settings {
        cmd.arg("settings");
    }
    match cmd.spawn() {
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
