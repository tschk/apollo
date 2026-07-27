#!/usr/bin/env bash
# Package apollo-ui as a macOS .app bundle.
#
# A bare binary does not register with LaunchServices, so it has no dock name,
# cannot be opened from Finder, and does not appear to screen-capture tooling.
# The bundle fixes all three.
#
# Usage: scripts/bundle-macos.sh [output-dir]   (default: target/release)

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="${1:-$ROOT/target/release}"
APP="$OUT_DIR/apollo.app"
BIN="$ROOT/target/release/apollo-ui"

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/ui/Cargo.toml" | head -1)"

if [ ! -x "$BIN" ]; then
  echo "error: $BIN not found — run: cargo build --release -p apollo-ui" >&2
  exit 1
fi

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/apollo"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key>              <string>apollo</string>
  <key>CFBundleDisplayName</key>       <string>apollo</string>
  <key>CFBundleExecutable</key>        <string>apollo</string>
  <key>CFBundleIdentifier</key>        <string>dev.apollo.ui</string>
  <key>CFBundleVersion</key>           <string>$VERSION</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundlePackageType</key>       <string>APPL</string>
  <key>LSMinimumSystemVersion</key>    <string>11.0</string>
  <key>NSHighResolutionCapable</key>   <true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
PLIST

# Ad-hoc signature so macOS will run it locally without a Gatekeeper prompt.
codesign --force --deep --sign - "$APP" 2>/dev/null || \
  echo "note: codesign unavailable; the app still runs but may prompt on first launch" >&2

# Register with LaunchServices so it is launchable and nameable right away.
LSREG=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
[ -x "$LSREG" ] && "$LSREG" -f "$APP" || true

echo "built $APP"
