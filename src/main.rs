// No console window when started normally on Windows (see `main`).
#![cfg_attr(windows, windows_subsystem = "windows")]

mod actions;
mod boot;
mod daemon;
mod db;
#[cfg(any(target_os = "macos", windows))]
mod hotkey;
mod images;
mod ipc;
mod keys;
mod settings;
mod tray;
mod ui;
mod update;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod mac;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
use mac as platform;
#[cfg(windows)]
use windows as platform;

use anyhow::{Result, bail};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

const HELP: &str = "clipr — keyboard-driven clipboard history

USAGE:
    clipr            run the background daemon (records history, global hotkey on macOS)
    clipr toggle     open/close the picker (starts the daemon if needed) — bind this to a key
    clipr pick       open the picker directly
    clipr store      save clipboard data from stdin (used by `wl-paste --watch`)
    clipr clear      delete the history, keeping pinned and saved clips
    clipr keys       print the path of the keyboard-shortcut file (keys.conf)
    clipr update     check for a newer version and install it
    clipr restart    stop and start clipr again
    clipr --version  print the version
";

/// Windows: run a console program without flashing a console window (which
/// would also take the focus, and with it close the picker).
pub fn hidden(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd
}

fn main() -> ExitCode {
    // As a GUI app on Windows we have no console; borrow the terminal's when
    // started from one, so `clipr --version` etc. still print. Not for the app
    // itself: attached, it would be closed along with that terminal window.
    #[cfg(windows)]
    if !matches!(std::env::args().nth(1).as_deref(), None | Some("daemon" | "pick")) {
        use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
        unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
    }
    let result = match std::env::args().nth(1).as_deref() {
        None | Some("daemon") => daemon::run(),
        Some("toggle") => toggle(),
        Some("pick") => ui::run(ui::Mode::OneShot),
        Some("store") => daemon::store_from_stdin(),
        Some("clear") => db::Db::open()
            .and_then(|db| db.clear(true))
            .map(|n| println!("cleared {n} clips (kept pinned & saved); undo in the picker with Mod+Shift+Z")),
        Some("keys" | "config") => {
            println!("{}", keys::ensure_config().display());
            Ok(())
        }
        Some("update") => update_cli(),
        Some("restart") => update::restart().map_err(Into::into),
        Some("-V" | "--version") => {
            println!("clipr {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("-h" | "--help" | "help") => {
            print!("{HELP}");
            Ok(())
        }
        Some(other) => Err(anyhow::anyhow!("unknown command {other:?}\n\n{HELP}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("clipr: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn toggle() -> Result<()> {
    if !ipc::daemon_running() {
        Command::new(std::env::current_exe()?)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let deadline = Instant::now() + Duration::from_secs(2);
        while !ipc::daemon_running() {
            if Instant::now() > deadline {
                bail!("daemon did not start");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    ipc::send("TOGGLE")
}

/// `clipr update`: same as Settings → Check for updates → Update now.
fn update_cli() -> Result<()> {
    let updater = update::Updater::default();
    let (tx, rx) = std::sync::mpsc::channel();
    updater.check(move || {
        let _ = tx.send(());
    });
    let _ = rx.recv();
    match updater.status() {
        update::Status::Available(v) => {
            println!("updating clipr {} → {v}…", update::current());
            updater.install();
            if let update::Status::Failed(e) = updater.status() {
                bail!(e);
            }
            println!("the installer is running; clipr restarts by itself");
            Ok(())
        }
        update::Status::UpToDate => {
            println!("clipr {} is the latest version", update::current());
            Ok(())
        }
        update::Status::Failed(e) => bail!(e),
        _ => Ok(()),
    }
}
