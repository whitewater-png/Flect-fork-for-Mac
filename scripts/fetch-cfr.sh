#!/bin/bash
# Fetch the pinned CFR decompiler into Flect-for-Mac's data folder and verify its SHA-256.
# The engine refuses any other jar, so this is the only supported way to install it.
set -euo pipefail

VERSION="0.152"
URL="https://github.com/leibnitz27/cfr/releases/download/${VERSION}/cfr-${VERSION}.jar"
SHA256="f686e8f3ded377d7bc87d216a90e9e9512df4156e75b06c655a16648ae8765b2"

DEST_DIR="${1:-${FLECT_HOME:-$HOME/Library/Application Support/Flect-for-Mac}/deps}"
DEST="$DEST_DIR/cfr.jar"

sha_of() { if command -v shasum >/dev/null; then shasum -a 256 "$1" | cut -d' ' -f1; else sha256sum "$1" | cut -d' ' -f1; fi; }

mkdir -p "$DEST_DIR"
if [ -f "$DEST" ] && [ "$(sha_of "$DEST")" = "$SHA256" ]; then
  echo "cfr.jar already present and verified: $DEST"
  exit 0
fi

tmp="$(mktemp "${TMPDIR:-/tmp}/cfr.XXXXXX")"
trap 'rm -f "$tmp"' EXIT
curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 --max-time 120 -o "$tmp" "$URL"
got="$(sha_of "$tmp")"
if [ "$got" != "$SHA256" ]; then
  echo "SHA-256 mismatch for $URL" >&2
  echo "  expected $SHA256" >&2
  echo "  got      $got" >&2
  exit 1
fi
chmod 644 "$tmp"
mv "$tmp" "$DEST"
trap - EXIT
echo "installed $DEST"
