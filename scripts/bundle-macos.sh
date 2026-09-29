#!/bin/sh
# Wraps a clipr binary into clipr.app.
# usage: bundle-macos.sh <binary> <output-dir> [version]
set -e
BIN="$1"; OUT="$2"; VERSION="${3:-0.0.0}"
APP="$OUT/clipr.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
cp "$BIN" "$APP/Contents/MacOS/clipr"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key><string>dev.clipr</string>
    <key>CFBundleName</key><string>clipr</string>
    <key>CFBundleExecutable</key><string>clipr</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>LSUIElement</key><true/>
    <key>LSMinimumSystemVersion</key><string>12.0</string>
</dict>
</plist>
PLIST

# macOS ties the Accessibility permission to the signature. An ad-hoc signature
# changes on every build (so macOS asks again); a stable self-signed
# "clipr-dev" code-signing certificate (see README) keeps the permission.
IDENTITY="${CLIPR_SIGN_IDENTITY:-clipr-dev}"
if security find-certificate -c "$IDENTITY" >/dev/null 2>&1; then
    codesign --force --sign "$IDENTITY" --identifier dev.clipr "$APP"
else
    codesign --force --sign - --identifier dev.clipr "$APP"
fi
echo "$APP"
