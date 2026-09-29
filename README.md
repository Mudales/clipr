# clipr

Light, keyboard-driven clipboard history for macOS and Linux (Hyprland), written in Rust.

- **History** of everything you copy: text and images (up to 1000 clips / 100 images, SQLite)
- **Saved** tab for clips you use often (numbered in the order you saved them)
- Fuzzy search as you type
- Paste straight into the app you were using, or **type it out** as keystrokes

## Install (macOS & Linux, no Rust needed)

```sh
curl -fsSL https://raw.githubusercontent.com/Mudales/clipr/master/install.sh | sh
```

Uninstall: `curl -fsSL https://raw.githubusercontent.com/Mudales/clipr/master/install.sh | sh -s -- --uninstall`

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

## Build from source

```sh
cargo build --release   # binary: target/release/clipr
```

## macOS

```sh
./scripts/install-macos.sh   # builds, installs ~/Applications/clipr.app and starts it
```

Press **⌘⇧V** to open the picker. Always start clipr as the app (not the bare binary
from a terminal), otherwise macOS asks the *terminal* for Accessibility permission.

**Keep the permission across rebuilds:** macOS ties it to the app's signature. Create a
self-signed certificate once — Keychain Access → Certificate Assistant → *Create a
Certificate…*, name `clipr-dev`, Identity Type *Self Signed Root*, Certificate Type
*Code Signing* — and `install-macos.sh` will sign with it. Without it, each rebuild
needs the permission re-granted (remove the old `clipr` entry first, or run
`tccutil reset Accessibility dev.clipr`).

- The first paste asks for **Accessibility** permission (System Settings → Privacy & Security → Accessibility) — needed to press ⌘V for you.
- macOS may ask once about clipboard access — choose *Always Allow*.
- Clips marked as secret by password managers are skipped.

## Linux (Hyprland / Omarchy)

Use the one-line installer above. It installs `~/.local/bin/clipr` (and `wtype`, which
sends the paste keystroke) and prints the lines to add to your Hyprland config.
`wl-clipboard` is used for instant capture and image history.

**Hyprland 0.56+ / Omarchy (Lua config; `hyprland.conf` is ignored):**

```lua
-- ~/.config/hypr/bindings.lua
o.bind("SUPER + SHIFT + V", "clipr clipboard history", os.getenv("HOME") .. "/.local/bin/clipr toggle")
-- ~/.config/hypr/autostart.lua
o.launch_on_start(os.getenv("HOME") .. "/.local/bin/clipr")
-- ~/.config/hypr/hyprland.lua (at the end)
o.window("^(clipr)$", { float = true, center = true })
```

**Older Hyprland (`hyprland.conf`):**

```
exec-once = ~/.local/bin/clipr
bind = SUPER SHIFT, V, exec, ~/.local/bin/clipr toggle
windowrule = float, class:^(clipr)$
windowrule = center, class:^(clipr)$
```

Then `hyprctl reload`. Terminals (Alacritty, Ghostty, kitty, ...) automatically get
Ctrl+Shift+V instead of Ctrl+V. Debug by running `~/.local/bin/clipr` in a terminal.

## Releasing

Push a tag (`git tag v0.1.1 && git push --tags`) — GitHub Actions builds the macOS
universal app and the Linux binary and attaches them to a release, which `install.sh` downloads.
