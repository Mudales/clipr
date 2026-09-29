#!/bin/sh
# Builds clipr, wraps it in ~/Applications/clipr.app and (re)starts it.
# A real .app gets its own Accessibility permission instead of inheriting the
# terminal's, and runs without a Dock icon.
set -e
cd "$(dirname "$0")/.."
cargo build --release

APP="$HOME/Applications/clipr.app"
pkill -x clipr 2>/dev/null || true
mkdir -p "$APP/Contents/MacOS"
cp target/release/clipr "$APP/Contents/MacOS/clipr"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key><string>dev.clipr</string>
    <key>CFBundleName</key><string>clipr</string>
    <key>CFBundleExecutable</key><string>clipr</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>0.1.0</string>
    <key>LSUIElement</key><true/>
    <key>LSMinimumSystemVersion</key><string>12.0</string>
</dict>
</plist>
PLIST
codesign --force --sign - --identifier dev.clipr "$APP"
open "$APP"
echo "Installed $APP"
