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
trap 'rm -rf "$TMP"' EXIT

# ---------------------------------------------------------------- macOS
MAC_APP="$HOME/Applications/clipr.app"
MAC_AGENT="$HOME/Library/LaunchAgents/dev.clipr.plist"

install_macos() {
    say "Downloading clipr for macOS"
    download "$(url_for clipr-macos-universal.zip)" "$TMP/clipr.zip"
    pkill -x clipr 2>/dev/null || true
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
    launchctl load "$MAC_AGENT"

    open "$MAC_APP"
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

install_linux() {
    case "$(uname -m)" in
        x86_64 | amd64) arch=x86_64 ;;
        *) die "no prebuilt binary for $(uname -m) yet — build from source: cargo install --git https://github.com/$REPO" ;;
    esac
    say "Downloading clipr for Linux ($arch)"
    download "$(url_for "clipr-linux-$arch.tar.gz")" "$TMP/clipr.tar.gz"
    tar -xzf "$TMP/clipr.tar.gz" -C "$TMP"
    pkill -x clipr 2>/dev/null || true
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

    nohup "$LINUX_BIN" >/dev/null 2>&1 &
    cat <<EOF

clipr is installed at $LINUX_BIN and running.

Add this to your Hyprland config (Omarchy: ~/.config/hypr/bindings.conf), then run
'hyprctl reload'. Pick another key if SUPER SHIFT V is taken:

  exec-once = $LINUX_BIN
  bind = SUPER SHIFT, V, exec, $LINUX_BIN toggle
  windowrule = float, class:^(clipr)$
  windowrule = center, class:^(clipr)$
EOF
}

uninstall_linux() {
    pkill -x clipr 2>/dev/null || true
    rm -f "$LINUX_BIN"
    say "Removed clipr (history kept in ~/.local/share/clipr). Remove the lines from your Hyprland config."
}

# ---------------------------------------------------------------- main
ACTION=install
[ "${1:-}" = "--uninstall" ] && ACTION=uninstall

case "$(uname -s)" in
    Darwin) "${ACTION}_macos" ;;
    Linux) "${ACTION}_linux" ;;
    *) die "unsupported OS: $(uname -s)" ;;
esac
