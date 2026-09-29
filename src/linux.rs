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
