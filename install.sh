#!/bin/sh
# clipr installer — downloads a prebuilt release, no Rust needed.
#
#   curl -fsSL https://raw.githubusercontent.com/Mudales/clipr/master/install.sh | sh
#   curl -fsSL https://raw.githubusercontent.com/Mudales/clipr/master/install.sh | sh -s -- --uninstall
#
# Set CLIPR_VERSION=v0.1.0 to pin a version (default: latest).
set -eu

REPO="Mudales/clipr"
VERSION="${CLIPR_VERSION:-latest}"

say() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
# Timestamped progress line (shows up in update.log for self-updates).
step() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*"; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

url_for() {
    if [ "$VERSION" = latest ]; then
        echo "https://github.com/$REPO/releases/latest/download/$1"
    else
        echo "https://github.com/$REPO/releases/download/$VERSION/$1"
    fi
}

download() { # url dest
    curl -fSL --progress-bar "$1" -o "$2" || die "download failed: $1"
}

TMP="$(mktemp -d)"

# macOS's pgrep/pkill skip their own parent processes unless given -a, and a
# self-update runs as a child of clipr: without -a it would never find clipr.
ANC=""
[ "$(uname -s)" = Darwin ] && ANC="-a"
running() { pgrep $ANC -x clipr | tr '\n' ' '; }

# Stops every running clipr and waits until it's really gone, so an old copy
# (e.g. one started again by hand mid-update) can't keep running.
stop_clipr() {
    pkill $ANC -x clipr 2>/dev/null || true
    i=0
    while pgrep $ANC -x clipr >/dev/null 2>&1 && [ $i -lt 30 ]; do
        sleep 0.1
        i=$((i + 1))
    done
    pkill -9 $ANC -x clipr 2>/dev/null || true
}
trap 'rm -rf "$TMP"' EXIT

# ---------------------------------------------------------------- macOS
MAC_APP="$HOME/Applications/clipr.app"
MAC_AGENT="$HOME/Library/LaunchAgents/dev.clipr.plist"

install_macos() {
    say "Downloading clipr for macOS"
    download "$(url_for clipr-macos-universal.zip)" "$TMP/clipr.zip"
    step "downloaded; running clipr: $(running)"
    stop_clipr
    step "stopped; running clipr: $(running)"
    mkdir -p "$HOME/Applications"
    rm -rf "$MAC_APP"
    ditto -x -k "$TMP/clipr.zip" "$HOME/Applications"
    xattr -dr com.apple.quarantine "$MAC_APP" 2>/dev/null || true

    say "Starting clipr at login"
    mkdir -p "$(dirname "$MAC_AGENT")"
    cat > "$MAC_AGENT" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>dev.clipr</string>
    <key>ProgramArguments</key>
    <array><string>/usr/bin/open</string><string>-a</string><string>$MAC_APP</string></array>
    <key>RunAtLoad</key><true/>
</dict>
</plist>
EOF
    launchctl unload "$MAC_AGENT" 2>/dev/null || true
    stop_clipr # in case an old copy was started while we were installing
    step "installed $("$MAC_APP/Contents/MacOS/clipr" --version 2>/dev/null)"
    launchctl load "$MAC_AGENT" # starts clipr (RunAtLoad)
    open "$MAC_APP"
    # Make sure the new clipr is really running; start it again if not.
    i=0
    while ! pgrep $ANC -x clipr >/dev/null 2>&1 && [ $i -lt 50 ]; do
        sleep 0.1
        i=$((i + 1))
    done
    if ! pgrep $ANC -x clipr >/dev/null 2>&1; then
        step "clipr didn't start, trying again"
        open -n "$MAC_APP"
    fi
    step "started; running clipr: $(running)"
    cat <<EOF

clipr is installed and running.

  • Press ⌘⇧V to open it.
  • Allow "clipr" under System Settings → Privacy & Security → Accessibility
    (needed to paste and type for you). After an update macOS may ask again —
    remove the old clipr entry and allow the new one.
EOF
}

uninstall_macos() {
    pkill -x clipr 2>/dev/null || true
    launchctl unload "$MAC_AGENT" 2>/dev/null || true
    rm -rf "$MAC_APP" "$MAC_AGENT"
    say "Removed clipr (history kept in ~/Library/Application Support/clipr)"
}

# ---------------------------------------------------------------- Linux
LINUX_BIN="$HOME/.local/bin/clipr"
LINUX_DESKTOP="$HOME/.local/share/applications/clipr.desktop"
LINUX_ICON="$HOME/.local/share/icons/hicolor/scalable/apps/clipr.svg"

install_linux() {
    case "$(uname -m)" in
        x86_64 | amd64) arch=x86_64 ;;
        *) die "no prebuilt binary for $(uname -m) yet — build from source: cargo install --git https://github.com/$REPO" ;;
    esac
    say "Downloading clipr for Linux ($arch)"
    download "$(url_for "clipr-linux-$arch.tar.gz")" "$TMP/clipr.tar.gz"
    tar -xzf "$TMP/clipr.tar.gz" -C "$TMP"
    stop_clipr
    mkdir -p "$(dirname "$LINUX_BIN")"
    install -m 755 "$TMP/clipr" "$LINUX_BIN"

    if [ -n "${WAYLAND_DISPLAY:-}" ] && ! command -v wtype >/dev/null 2>&1; then
        if command -v pacman >/dev/null 2>&1; then
            say "Installing wtype (sends the paste keystroke)"
            sudo pacman -S --needed --noconfirm wtype || say "could not install wtype — run: sudo pacman -S wtype"
        else
            say "Please install 'wtype' with your package manager (needed to paste)"
        fi
    fi

    install_launcher_entry
    stop_clipr # in case an old copy was started while we were installing
    nohup "$LINUX_BIN" >/dev/null 2>&1 &
    command -v wl-paste >/dev/null 2>&1 || say "Tip: install 'wl-clipboard' for image history and instant capture"

    echo
    echo "clipr is installed at $LINUX_BIN and running."
    echo
    configure_hyprland
}

# Adds the shortcut (SUPER+SHIFT+V), start at login and the window rule to the
# Hyprland config, so there's nothing to edit by hand. Every added line is
# marked, so --uninstall can take exactly those out again. Skipped when the
# config already mentions clipr, or with CLIPR_NO_CONFIG=1.
HYPR="$HOME/.config/hypr"
MARK="added by the clipr installer"

configure_hyprland() {
    [ -d "$HYPR" ] || return 0
    if [ -n "${CLIPR_NO_CONFIG:-}" ]; then
        echo "Skipped the Hyprland config (CLIPR_NO_CONFIG). See the README for the lines to add."
        return 0
    fi
    if grep -qs "clipr" "$HYPR"/*.lua "$HYPR"/*.conf; then
        echo "Your Hyprland config already starts clipr — nothing to add."
        return 0
    fi
    key_taken=""
    if [ -f "$HYPR/hyprland.lua" ]; then
        # Hyprland 0.56+ / Omarchy: Lua config (hyprland.conf is ignored).
        grep -qsE '"SUPER ?\+ ?SHIFT ?\+ ?V"' "$HYPR"/*.lua "$HOME"/.local/share/omarchy/default/hypr/*.lua && key_taken=1
        [ -f "$HYPR/bindings.lua" ] && bindings="$HYPR/bindings.lua" || bindings="$HYPR/hyprland.lua"
        [ -f "$HYPR/autostart.lua" ] && autostart="$HYPR/autostart.lua" || autostart="$HYPR/hyprland.lua"
        [ -n "$key_taken" ] || add_line "$bindings" \
            "o.bind(\"SUPER + SHIFT + V\", \"clipr clipboard history\", os.getenv(\"HOME\") .. \"/.local/bin/clipr toggle\") -- $MARK"
        add_line "$autostart" "o.launch_on_start(os.getenv(\"HOME\") .. \"/.local/bin/clipr\") -- $MARK"
        add_line "$HYPR/hyprland.lua" \
            "o.window(\"^(clipr)\$\", { float = true, center = true, stay_focused = true, tag = \"-default-opacity\", opacity = \"1.0 1.0\" }) -- $MARK"
    elif [ -f "$HYPR/hyprland.conf" ]; then
        grep -qsiE '^\s*bind\s*=\s*SUPER\s*SHIFT\s*,\s*V\s*,' "$HYPR"/*.conf && key_taken=1
        conf="$HYPR/hyprland.conf"
        add_line "$conf" "exec-once = $LINUX_BIN # $MARK"
        [ -n "$key_taken" ] || add_line "$conf" "bind = SUPER SHIFT, V, exec, $LINUX_BIN toggle # $MARK"
        add_line "$conf" "windowrule = float, class:^(clipr)\$ # $MARK"
        add_line "$conf" "windowrule = center, class:^(clipr)\$ # $MARK"
        add_line "$conf" "windowrule = stayfocused, class:^(clipr)\$ # $MARK"
    else
        return 0
    fi
    command -v hyprctl >/dev/null 2>&1 && hyprctl reload >/dev/null 2>&1 || true
    if [ -n "$key_taken" ]; then
        echo "Added clipr to your Hyprland config. SUPER+SHIFT+V is already used, so pick a key"
        echo "for '$LINUX_BIN toggle' yourself (or open clipr from the tray icon / app launcher)."
    else
        echo "Added clipr to your Hyprland config: press SUPER+SHIFT+V to open it."
    fi
}

# Appends a line to a config file, keeping a one-time backup of the original.
add_line() {
    [ -f "$1.before-clipr" ] || cp "$1" "$1.before-clipr" 2>/dev/null || true
    # Start on a new line if the file doesn't end with one.
    [ -s "$1" ] && [ -n "$(tail -c 1 "$1")" ] && printf '\n' >> "$1"
    printf '%s\n' "$2" >> "$1"
}

# Makes clipr show up in app launchers (Omarchy's SUPER+SPACE, rofi, walker…).
install_launcher_entry() {
    mkdir -p "$(dirname "$LINUX_DESKTOP")" "$(dirname "$LINUX_ICON")"
    cat > "$LINUX_ICON" <<'SVG'
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256">
  <defs>
    <linearGradient id="bg" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#2f7df6"/>
      <stop offset="1" stop-color="#0a55cc"/>
    </linearGradient>
  </defs>
  <rect x="16" y="16" width="224" height="224" rx="52" fill="url(#bg)"/>
  <!-- clipboard board -->
  <rect x="70" y="58" width="116" height="146" rx="16" fill="#ffffff"/>
  <!-- clip -->
  <rect x="98" y="44" width="60" height="30" rx="10" fill="#dbe7fb" stroke="#0a55cc" stroke-width="6"/>
  <!-- history lines -->
  <rect x="90" y="96" width="76" height="12" rx="6" fill="#2f7df6"/>
  <rect x="90" y="124" width="60" height="12" rx="6" fill="#9dbdf2"/>
  <rect x="90" y="152" width="68" height="12" rx="6" fill="#9dbdf2"/>
</svg>
SVG
    cat > "$LINUX_DESKTOP" <<EOF
[Desktop Entry]
Type=Application
Name=clipr
GenericName=Clipboard history
Comment=Search and paste your clipboard history
Exec=$LINUX_BIN toggle
Icon=clipr
Terminal=false
Categories=Utility;
Keywords=clipboard;history;paste;copy;
StartupNotify=false
EOF
    command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q "$(dirname "$LINUX_DESKTOP")" || true
}

uninstall_linux() {
    pkill -x clipr 2>/dev/null || true
    rm -f "$LINUX_BIN" "$LINUX_DESKTOP" "$LINUX_ICON"
    for f in "$HYPR"/*.lua "$HYPR"/*.conf; do
        [ -f "$f" ] && grep -q "$MARK" "$f" && sed -i "/$MARK/d" "$f"
    done
    command -v hyprctl >/dev/null 2>&1 && hyprctl reload >/dev/null 2>&1 || true
    say "Removed clipr (history kept in ~/.local/share/clipr)."
    grep -qs "clipr" "$HYPR"/*.lua "$HYPR"/*.conf && echo "Your Hyprland config still has clipr lines you added yourself." || true
}

# ---------------------------------------------------------------- main
ACTION=install
[ "${1:-}" = "--uninstall" ] && ACTION=uninstall

case "$(uname -s)" in
    Darwin) "${ACTION}_macos" ;;
    Linux) "${ACTION}_linux" ;;
    *) die "unsupported OS: $(uname -s)" ;;
esac
