mod daemon;
mod db;
mod images;
mod ipc;
mod ui;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod mac;

#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
use mac as platform;

use anyhow::{Result, bail};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

const HELP: &str = "clipr — keyboard-driven clipboard history

USAGE:
    clipr            run the background daemon (records history, global hotkey on macOS)
    clipr toggle     open/close the picker (starts the daemon if needed) — bind this to a key
    clipr pick       open the picker directly
    clipr store      save clipboard data from stdin (used by `wl-paste --watch`)
    clipr --version  print the version
";

fn main() -> ExitCode {
    let result = match std::env::args().nth(1).as_deref() {
        None | Some("daemon") => daemon::run(),
        Some("toggle") => toggle(),
        Some("pick") => ui::run(ui::Mode::OneShot),
        Some("store") => daemon::store_from_stdin(),
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
