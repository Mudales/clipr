//! Linux glue. Keystrokes go through `wtype` on Wayland (Hyprland supports the
//! virtual-keyboard protocol) and `xdotool` on X11.

use anyhow::{Context, Result, bail};
use std::process::Command;

/// Terminals paste with Ctrl+Shift+V instead of Ctrl+V.
const TERMINALS: &[&str] = &[
    "alacritty",
    "kitty",
    "foot",
    "ghostty",
    "com.mitchellh.ghostty",
    "wezterm",
    "org.wezfurlong.wezterm",
    "konsole",
    "gnome-terminal",
    "org.gnome.terminal",
    "xterm",
];

fn wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
}

fn run(cmd: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(cmd)
        .args(args)
        .status()
        .with_context(|| format!("running `{cmd}` (is it installed?)"))?;
    if !status.success() {
        bail!("`{cmd}` failed with {status}");
    }
    Ok(())
}

/// Window class of the focused window (Hyprland).
pub fn active_window_class() -> Option<String> {
    let out = Command::new("hyprctl").args(["activewindow", "-j"]).output().ok()?;
    let json = String::from_utf8_lossy(&out.stdout).to_lowercase();
    // Cheap parse: find `"class": "<name>"`.
    json.split("\"class\":").nth(1)?.split('"').nth(1).map(str::to_owned)
}

fn active_window_is_terminal() -> bool {
    active_window_class().is_some_and(|class| TERMINALS.contains(&class.as_str()))
}

pub fn send_paste() -> Result<()> {
    let terminal = active_window_is_terminal();
    if wayland() {
        if terminal {
            run("wtype", &["-M", "ctrl", "-M", "shift", "v", "-m", "shift", "-m", "ctrl"])
        } else {
            run("wtype", &["-M", "ctrl", "v", "-m", "ctrl"])
        }
    } else {
        run("xdotool", &["key", "--clearmodifiers", if terminal { "ctrl+shift+v" } else { "ctrl+v" }])
    }
}

pub fn type_text(text: &str) -> Result<()> {
    if wayland() {
        run("wtype", &["--", text])
    } else {
        run("xdotool", &["type", "--clearmodifiers", "--", text])
    }
}

/// Puts the most recent clip back on the clipboard after the app that owned
/// it closed (Wayland then leaves the clipboard empty). `wl-copy` keeps
/// serving it in the background.
pub fn restore_clipboard(db: &crate::db::Db) -> Result<()> {
    use std::io::Write;
    // The text and image watchers both report "empty"; only restore once,
    // and only if the clipboard really has nothing in any format.
    let types = Command::new("wl-paste").arg("--list-types").output();
    if types.is_ok_and(|o| o.status.success() && !o.stdout.is_empty()) {
        return Ok(());
    }
    let marker = crate::db::data_dir().join("restored");
    let recent = std::fs::metadata(&marker)
        .and_then(|m| m.modified())
        .is_ok_and(|t| t.elapsed().unwrap_or_default() < std::time::Duration::from_secs(2));
    if recent {
        return Ok(());
    }
    let _ = std::fs::write(&marker, "");
    let Some(payload) = db.latest()? else { return Ok(()) };
    let (mime, bytes) = match payload {
        crate::db::Payload::Text(t) => ("text/plain;charset=utf-8", t.into_bytes()),
        crate::db::Payload::Image(png) => ("image/png", png),
    };
    let mut child = Command::new("wl-copy")
        .args(["--type", mime])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("running wl-copy")?;
    child.stdin.take().unwrap().write_all(&bytes)?;
    child.wait()?; // wl-copy forks to serve, the parent returns at once
    Ok(())
}

/// Starts `wl-paste --watch clipr store` for text and for images. Returns false
/// if wl-paste isn't available. The watchers die with the daemon.
pub fn spawn_wl_watchers() -> bool {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe().unwrap_or_else(|_| "clipr".into());
    let mut ok = true;
    for kind in ["text", "image"] {
        let mut cmd = Command::new("wl-paste");
        cmd.args(["--type", kind, "--watch"]).arg(&exe).arg("store");
        // SAFETY: prctl is async-signal-safe; only asks for SIGTERM when we exit.
        unsafe {
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
        if let Err(e) = cmd.spawn() {
            eprintln!("clipr: wl-paste not usable ({e}), falling back to polling");
            ok = false;
            break;
        }
    }
    ok
}
