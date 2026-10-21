#!/bin/bash
# Build Flect-for-Mac.app from source (Apple Silicon, macOS 14+), ad-hoc signed. No Apple Developer account needed.
#   scripts/build.sh            -> dist/Flect-for-Mac.app
#   scripts/build.sh --install  -> also copies the app to ~/Applications and installs the Quick Actions
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT=$PWD
APP="$ROOT/dist/Flect-for-Mac.app"
BUNDLE_ID="${FLECT_BUNDLE_ID:-io.github.whitewater-png.flect-for-mac}"
VERSION="${FLECT_VERSION:-0.1.2}"
VERSION="${VERSION#v}"   # a tag such as v0.1.1 gives 0.1.1 (macOS shows the version without the "v")

command -v cargo >/dev/null || { echo "cargo not found: brew install rustup && rustup-init" >&2; exit 1; }
command -v swift >/dev/null || { echo "swift not found: install Xcode or the Command Line Tools" >&2; exit 1; }

# The macOS 27 SDK declares @State etc. as macros whose plugin (SwiftUIMacros) ships only with full Xcode.
# With just the Command Line Tools, fall back to the newest SDK that still compiles SwiftUI.
if [ -z "${SDKROOT:-}" ]; then
  probe() {
    local dir; dir="$(mktemp -d)"
    printf 'import SwiftUI\nstruct P: View { @State private var x = false; var body: some View { Toggle("", isOn: $x) } }\n' > "$dir/p.swift"
    swiftc -typecheck -sdk "$1" "$dir/p.swift" >/dev/null 2>&1; local rc=$?
    rm -rf "$dir"; return $rc
  }
  default_sdk="$(xcrun --show-sdk-path 2>/dev/null || true)"
  if [ -n "$default_sdk" ] && ! probe "$default_sdk"; then
    for sdk in $(ls -d /Library/Developer/CommandLineTools/SDKs/MacOSX[0-9]*.[0-9]*.sdk 2>/dev/null | sort -rV); do
      if probe "$sdk"; then export SDKROOT="$sdk"; echo "== Using $(basename "$sdk") (the default SDK cannot compile SwiftUI without full Xcode)"; break; fi
    done
  fi
fi

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
cp core/target/release/flect "$APP/Contents/Resources/flect"
# SwiftPM resource bundle for FlectCore (string tables)
BIN="$(cd app && swift build -c release --show-bin-path)"
for b in "$BIN"/*.bundle; do [ -e "$b" ] && cp -R "$b" "$APP/Contents/Resources/"; done

# The dependency scripts travel with the app so a downloaded build can use them too.
mkdir -p "$APP/Contents/Resources/scripts"
cp scripts/setup-deps.sh scripts/fetch-cfr.sh scripts/fetch-whisper-model.sh "$APP/Contents/Resources/scripts/"
chmod +x "$APP/Contents/Resources/scripts/"*.sh

# App icon: assets/icon/AppIcon-1024.png (rendered from AppIcon.svg) -> AppIcon.icns
ICON_KEY=""
if [ -f assets/icon/AppIcon-1024.png ] && command -v iconutil >/dev/null && command -v sips >/dev/null; then
  iconset="$(mktemp -d)/AppIcon.iconset"; mkdir -p "$iconset"
  for size in 16 32 128 256 512; do
    sips -z $size $size assets/icon/AppIcon-1024.png --out "$iconset/icon_${size}x${size}.png" >/dev/null
    sips -z $((size*2)) $((size*2)) assets/icon/AppIcon-1024.png --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
  done
  iconutil -c icns "$iconset" -o "$APP/Contents/Resources/AppIcon.icns" && ICON_KEY="<key>CFBundleIconFile</key><string>AppIcon</string>"
fi

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>Flect-for-Mac</string>
  <key>CFBundleDisplayName</key><string>Flect</string>
  <key>CFBundleIdentifier</key><string>${BUNDLE_ID}</string>
  <key>CFBundleExecutable</key><string>Flect-for-Mac</string>
  ${ICON_KEY}
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleLocalizations</key><array><string>en</string><string>ja</string><string>zh-Hans</string></array>
  <!-- Files / folders dropped on the Dock icon or on Flect.app (or opened with it) open the action picker.
       Rank "None": Flect shows up under Open With but never becomes anyone's default app. -->
  <key>CFBundleDocumentTypes</key><array><dict>
    <key>CFBundleTypeName</key><string>Files and folders</string>
    <key>CFBundleTypeRole</key><string>Viewer</string>
    <key>LSHandlerRank</key><string>None</string>
    <key>LSItemContentTypes</key><array><string>public.item</string><string>public.folder</string></array>
  </dict></array>
  <!-- Services menu entry "Process with Flect…" (title localized by */ServicesMenu.strings, handler: ServicesProvider.processWithFlect) -->
  <key>NSServices</key><array><dict>
    <key>NSMenuItem</key><dict><key>default</key><string>Process with Flect…</string></dict>
    <key>NSMessage</key><string>processWithFlect</string>
    <key>NSPortName</key><string>Flect-for-Mac</string>
    <key>NSSendFileTypes</key><array><string>public.item</string></array>
  </dict></array>
</dict></plist>
PLIST

# Localized Services menu title
for lp in assets/services/*.lproj; do
  mkdir -p "$APP/Contents/Resources/$(basename "$lp")"
  cp "$lp/ServicesMenu.strings" "$APP/Contents/Resources/$(basename "$lp")/"
done

echo "== Ad-hoc signature"
codesign --force --sign - "$APP/Contents/Resources/flect-engine"
codesign --force --sign - "$APP/Contents/Resources/flect-tools"
codesign --force --sign - "$APP/Contents/Resources/flect"
codesign --force --deep --sign - "$APP"
codesign --verify --deep --strict "$APP"
echo "built: $APP"

if [ "${1:-}" = "--install" ]; then
  mkdir -p "$HOME/Applications"
  rm -rf "$HOME/Applications/Flect-for-Mac.app"
  cp -R "$APP" "$HOME/Applications/"
  "$HOME/Applications/Flect-for-Mac.app/Contents/Resources/flect-tools" --out "$HOME/Library/Services"
  echo "Quick Actions installed to ~/Library/Services (restart Finder: killall Finder)"
  echo
  # Tools Flect calls but does not bundle: ask before installing anything (non-interactive runs only report).
  if [ -t 0 ]; then
    "$HOME/Applications/Flect-for-Mac.app/Contents/Resources/scripts/setup-deps.sh" || true
  else
    "$HOME/Applications/Flect-for-Mac.app/Contents/Resources/scripts/setup-deps.sh" --check || true
  fi
fi
