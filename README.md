# clipr

Light, keyboard-driven clipboard history for macOS and Linux (Hyprland), written in Rust.

- **History** of everything you copy (text, up to 1000 clips, stored in SQLite)
- **Saved** tab for clips you use often (numbered in the order you saved them)
- Fuzzy search as you type
- Paste straight into the app you were using, or **type it out** as keystrokes

## Keys (in the picker)

| Key | Action |
|---|---|
| type | search |
| ↑ ↓ PgUp PgDn | move |
| Enter | paste into the previous app |
| ⌘/Ctrl + 1–9 | paste item 1–9 |
| Shift + Enter | copy only |
| ⌘/Ctrl + Enter | type it out (for fields that block paste) |
| ⌘/Ctrl + S | save / unsave |
| ⌘/Ctrl + P | pin / unpin (pinned stay at the top of History) |
| ⌘/Ctrl + D | delete |
| Tab | switch History ⇄ Saved |
| Esc | close |

## Build

```sh
cargo build --release   # binary: target/release/clipr
```

## macOS

```sh
./scripts/install-macos.sh   # builds, installs ~/Applications/clipr.app and starts it
```

Press **⌘⇧V** to open the picker. Always start clipr as the app (not the bare binary
from a terminal), otherwise macOS asks the *terminal* for Accessibility permission.
Rebuilding changes the app's signature, so macOS may ask for the permission again.

- The first paste asks for **Accessibility** permission (System Settings → Privacy & Security → Accessibility) — needed to press ⌘V for you.
- macOS may ask once about clipboard access — choose *Always Allow*.
- Clips marked as secret by password managers are skipped.

## Linux (Hyprland / Omarchy)

```sh
sudo pacman -S --needed rustup base-devel wtype   # wtype sends the paste keystroke
rustup default stable
git clone https://github.com/Mudales/clipr && cd clipr
cargo install --path .                             # installs ~/.cargo/bin/clipr
```

Add to `~/.config/hypr/hyprland.conf` (on Omarchy: `~/.config/hypr/bindings.conf` for the bind,
and check `SUPER SHIFT V` isn't already used):

```
exec-once = ~/.cargo/bin/clipr
bind = SUPER SHIFT, V, exec, ~/.cargo/bin/clipr toggle
windowrule = float, class:^(clipr)$
windowrule = center, class:^(clipr)$
```

Then `hyprctl reload` and start it once by hand: `~/.cargo/bin/clipr &`.
(The window-rule syntax changed across Hyprland versions — adjust if `hyprctl reload` complains.)

Terminals (Alacritty, Ghostty, kitty, …) automatically get Ctrl+Shift+V instead of Ctrl+V.
Debug by running `~/.cargo/bin/clipr` in a terminal and watching its output.
