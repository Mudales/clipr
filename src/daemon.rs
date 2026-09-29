//! The background process: records clipboard history, owns the clipboard when
//! pasting, launches the picker popup and sends the paste keystroke.

use crate::db::Db;
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
        let _hotkey = platform::register_hotkey(toggle_popup)?;
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

fn watch_clipboard() -> Result<()> {
    let db = Db::open()?;
    let mut clipboard = arboard::Clipboard::new()?;
    #[cfg(not(target_os = "macos"))]
    let mut last: Option<String> = None;
    #[cfg(target_os = "macos")]
    let mut last_count = platform::change_count() - 1;

    loop {
        thread::sleep(POLL);

        // macOS exposes a change counter, so we only read the contents when
        // something was actually copied (and can see re-copies of the same text).
        #[cfg(target_os = "macos")]
        {
            let count = platform::change_count();
            if count == last_count {
                continue;
            }
            last_count = count;
            if platform::should_skip_current() {
                continue;
            }
            if let Ok(text) = clipboard.get_text() {
                log_err("db", db.add(&text));
            }
        }

        #[cfg(not(target_os = "macos"))]
        if let Ok(text) = clipboard.get_text() {
            if last.as_deref() != Some(text.as_str()) {
                log_err("db", db.add(&text));
                last = Some(text);
            }
        }
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
    let id: i64 = arg.parse().with_context(|| format!("bad command {line:?}"))?;
    let Some(text) = db.get(id)? else { return Ok(()) };
    db.add(&text)?; // move to top of history

    match cmd {
        "COPY" => {
            clipboard.set_text(&text)?;
            wait_for_popup_exit();
        }
        "PASTE" => {
            clipboard.set_text(&text)?;
            wait_for_popup_exit();
            platform::send_paste()?;
        }
        "TYPE" => {
            wait_for_popup_exit();
            platform::type_text(&text)?;
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
