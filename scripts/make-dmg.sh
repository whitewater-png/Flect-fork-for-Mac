#!/bin/bash
# Build the disk image people download: the app, a shortcut to Applications to drag it onto, a guide drawn
# into the window background and a text file with the same steps (for the macOS "cannot verify the developer" warning).
#   scripts/build.sh && scripts/make-dmg.sh      -> dist/Flect-for-Mac-macos-arm64.dmg
#   scripts/make-dmg.sh <path/to/Flect-for-Mac.app> <output.dmg>
# dmgbuild (MIT) writes the Finder layout directly; it is installed once into .build/dmg-venv from
# scripts/dmg/requirements.txt (pinned versions and hashes; needs the network that first time).
set -euo pipefail
cd "$(dirname "$0")/.."
APP="${1:-dist/Flect-for-Mac.app}"
OUT="${2:-dist/Flect-for-Mac-macos-arm64.dmg}"
[ -d "$APP" ] || { echo "app not found: $APP (run scripts/build.sh first)" >&2; exit 1; }
[ -f "$APP/Contents/Resources/AppIcon.icns" ] || { echo "no AppIcon.icns in $APP" >&2; exit 1; }
APP="$(cd "$(dirname "$APP")" && pwd)/$(basename "$APP")"

VENV=.build/dmg-venv
REQ=scripts/dmg/requirements.txt
REQ_SHA="$(shasum -a 256 "$REQ" | cut -d' ' -f1)"
if [ ! -x "$VENV/bin/dmgbuild" ] || ! "$VENV/bin/python" -c 1 2>/dev/null || [ "$(cat "$VENV/.requirements.sha256" 2>/dev/null)" != "$REQ_SHA" ]; then
  PY=""
  for c in python3.14 python3.13 python3.12 python3.11 python3.10 python3; do
    if command -v "$c" >/dev/null && "$c" -c 'import sys; sys.exit(0 if sys.version_info >= (3, 10) else 1)'; then PY="$c"; break; fi
  done
  [ -n "$PY" ] || { echo "Python 3.10 or newer is needed to build the dmg" >&2; exit 1; }
  echo "== Installing dmgbuild (pinned) into $VENV"
  rm -rf "$VENV"
  "$PY" -m venv "$VENV"
  "$VENV/bin/python" -m pip install --quiet --disable-pip-version-check --require-hashes -r "$REQ"
  echo "$REQ_SHA" > "$VENV/.requirements.sha256"
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
echo "== Background"
swift scripts/dmg/make-background.swift "$WORK/background.png"
README="$WORK/開けないときは.txt"
cp scripts/dmg/README.txt "$README"

echo "== Disk image"
mkdir -p "$(dirname "$OUT")"
rm -f "$OUT"
"$VENV/bin/dmgbuild" -s scripts/dmg/settings.py \
  -D app="$APP" -D readme="$README" -D background="$WORK/background.png" -D icon="$APP/Contents/Resources/AppIcon.icns" \
  "Flect-for-Mac" "$OUT"
echo "built: $OUT"
shasum -a 256 "$OUT"
