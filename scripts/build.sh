#!/bin/bash
# Build Flect-for-Mac.app from source (Apple Silicon, macOS 14+), ad-hoc signed. No Apple Developer account needed.
#   scripts/build.sh            -> dist/Flect-for-Mac.app
#   scripts/build.sh --install  -> also copies the app to ~/Applications and installs the Quick Actions
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT=$PWD
APP="$ROOT/dist/Flect-for-Mac.app"
BUNDLE_ID="${FLECT_BUNDLE_ID:-io.github.flect-for-mac}"
VERSION="${FLECT_VERSION:-0.1.0}"

command -v cargo >/dev/null || { echo "cargo not found: brew install rustup && rustup-init" >&2; exit 1; }
command -v swift >/dev/null || { echo "swift not found: install Xcode or the Command Line Tools" >&2; exit 1; }

echo "== Rust engine"
(cd core && cargo build --release --locked)
echo "== Swift app"
(cd app && swift build -c release)

echo "== Bundle"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$(cd app && swift build -c release --show-bin-path)/Flect-for-Mac" "$APP/Contents/MacOS/Flect-for-Mac"
cp core/target/release/flect-engine "$APP/Contents/Resources/flect-engine"
cp core/target/release/flect-tools "$APP/Contents/Resources/flect-tools"
# SwiftPM resource bundle for FlectCore (string tables)
BIN="$(cd app && swift build -c release --show-bin-path)"
for b in "$BIN"/*.bundle; do [ -e "$b" ] && cp -R "$b" "$APP/Contents/Resources/"; done

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>Flect-for-Mac</string>
  <key>CFBundleDisplayName</key><string>Flect</string>
  <key>CFBundleIdentifier</key><string>${BUNDLE_ID}</string>
  <key>CFBundleExecutable</key><string>Flect-for-Mac</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
  <key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST

echo "== Ad-hoc signature"
codesign --force --sign - "$APP/Contents/Resources/flect-engine"
codesign --force --sign - "$APP/Contents/Resources/flect-tools"
codesign --force --deep --sign - "$APP"
codesign --verify --deep --strict "$APP"
echo "built: $APP"

if [ "${1:-}" = "--install" ]; then
  mkdir -p "$HOME/Applications"
  rm -rf "$HOME/Applications/Flect-for-Mac.app"
  cp -R "$APP" "$HOME/Applications/"
  "$HOME/Applications/Flect-for-Mac.app/Contents/Resources/flect-tools" --out "$HOME/Library/Services"
  echo "Quick Actions installed to ~/Library/Services (restart Finder: killall Finder)"
fi
