#!/bin/bash
# Install a whisper.cpp (ggml) speech model for subtitles, after checking its pinned SHA-256.
#   scripts/fetch-whisper-model.sh [tiny|base|small|medium|large-v3|turbo]   (default: small)
# The file goes to ~/Library/Application Support/Flect-for-Mac/deps/whisper/ (or $FLECT_HOME/deps/whisper/).
# The same hashes are pinned in core/flect-core/src/subtitles.rs; the engine refuses any other file.
set -euo pipefail
name="${1:-small}"
case "$name" in
  tiny)     file=ggml-tiny.bin;           sha=be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21 ;;
  base)     file=ggml-base.bin;           sha=60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe ;;
  small)    file=ggml-small.bin;          sha=1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b ;;
  medium)   file=ggml-medium.bin;         sha=6c14d5adee5f86394037b4e4e8b59f1673b6cee10e3cf0b11bbdbee79c156208 ;;
  large-v3) file=ggml-large-v3.bin;       sha=64d182b440b98d5203c4f9bd541544d84c605196c4f7b845dfa11fb23594d1e2 ;;
  turbo)    file=ggml-large-v3-turbo.bin; sha=1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69 ;;
  *) echo "unknown model '$name' (tiny, base, small, medium, large-v3, turbo)" >&2; exit 2 ;;
esac
base="${FLECT_HOME:-$HOME/Library/Application Support/Flect-for-Mac}"
dir="$base/deps/whisper"
mkdir -p "$dir"; chmod 700 "$base" "$base/deps" "$dir" 2>/dev/null || true
dest="$dir/$file"
if [ -f "$dest" ] && [ "$(shasum -a 256 "$dest" | cut -d' ' -f1)" = "$sha" ]; then echo "already installed: $dest"; exit 0; fi
tmp="$(mktemp "$dir/.download.XXXXXX")"
trap 'rm -f "$tmp"' EXIT
echo "downloading $file ..."
curl -fL --proto '=https' --tlsv1.2 -o "$tmp" "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/$file"
actual="$(shasum -a 256 "$tmp" | cut -d' ' -f1)"
if [ "$actual" != "$sha" ]; then echo "checksum mismatch for $file (got $actual); not installed" >&2; exit 1; fi
chmod 600 "$tmp"; mv "$tmp" "$dest"; trap - EXIT
echo "installed: $dest"
