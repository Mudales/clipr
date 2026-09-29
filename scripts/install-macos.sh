#!/bin/sh
# Development install: builds from source, installs ~/Applications/clipr.app
# and (re)starts it. End users should use install.sh instead.
set -e
cd "$(dirname "$0")/.."
cargo build --release
pkill -x clipr 2>/dev/null || true
mkdir -p "$HOME/Applications"
scripts/bundle-macos.sh target/release/clipr "$HOME/Applications" "$(cargo pkgid | cut -d'#' -f2)"
open "$HOME/Applications/clipr.app"
