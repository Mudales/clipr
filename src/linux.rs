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

fn active_window_is_terminal() -> bool {
    let Ok(out) = Command::new("hyprctl").args(["activewindow", "-j"]).output() else {
        return false;
    };
    let json = String::from_utf8_lossy(&out.stdout).to_lowercase();
    // Cheap parse: find `"class": "<name>"`.
    json.split("\"class\":")
        .nth(1)
        .and_then(|rest| rest.split('"').nth(1))
        .is_some_and(|class| TERMINALS.contains(&class))
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
