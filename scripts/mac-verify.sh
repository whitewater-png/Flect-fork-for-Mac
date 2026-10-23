#!/bin/bash
# Prepares a Mac for the manual check and runs the part of it that a script can do.
# Installs nothing (it only tells you the brew command for what is missing) and contacts no third-party site.
#
#   scripts/mac-verify.sh                    record the environment, make ~/FlectTest, run the download check
#   scripts/mac-verify.sh --engine PATH      use that flect-engine for the download check
#   scripts/mac-verify.sh --clean            remove ~/FlectTest
#
# The screen checks (Finder, dialogs, menus) are in docs/mac-port/19-mac-verification-checklist.ja.md.
# Written for the bash that ships with macOS (3.2).
set -u
ORIG="$PWD"
cd "$(dirname "$0")/.." || exit 1
ROOT="$PWD"
TEST_DIR="$HOME/FlectTest"
ENGINE=""

while [ $# -gt 0 ]; do
  case "$1" in
    --clean) rm -rf "$TEST_DIR"; echo "removed $TEST_DIR"; exit 0 ;;
    --engine) shift; ENGINE="${1:-}" ;;
    -h|--help) sed -n 2,10p "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done

case "$ENGINE" in ""|/*) ;; *) ENGINE="$ORIG/$ENGINE" ;; esac   # a relative path was given from where you started

[ "$(uname -s)" = "Darwin" ] || echo "note: this is not macOS; the screen checks need a Mac, but the rest still runs."

tool() { for d in /opt/homebrew/bin /usr/local/bin; do [ -x "$d/$1" ] && { echo "$d/$1"; return; }; done; }
line() { printf '%-26s %s\n' "$1" "$2"; }

echo "== 1. Environment (paste this into the report)"
line "date" "$(date '+%Y-%m-%d %H:%M')"
line "macOS" "$(sw_vers -productVersion 2>/dev/null || uname -r) ($(sw_vers -buildVersion 2>/dev/null || echo ?))"
line "chip" "$(uname -m) / $(sysctl -n machdep.cpu.brand_string 2>/dev/null || echo ?)"
line "swift" "$( (swift --version 2>/dev/null | head -1) || true)"
line "git branch / commit" "$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo ?) / $(git rev-parse --short HEAD 2>/dev/null || echo ?)"
for t in ffmpeg ffprobe 7zz yt-dlp; do
  p="$(tool "$t")"
  if [ -n "$p" ]; then
    case "$t" in yt-dlp) v="$("$p" --version 2>/dev/null | head -1)" ;; 7zz) v="$("$p" 2>/dev/null | sed -n 2p)" ;; *) v="$("$p" -version 2>/dev/null | head -1)" ;; esac
    line "$t" "$p   $v"
  else
    case "$t" in
      yt-dlp) hint="brew install yt-dlp   (only for the web download)" ;;
      7zz) hint="brew install sevenzip" ;;
      *) hint="brew install ffmpeg-full" ;;
    esac
    line "$t" "NOT FOUND in /opt/homebrew/bin or /usr/local/bin  ->  $hint"
  fi
done
f="$(tool ffmpeg)"
[ -n "$f" ] && line "videotoolbox" "$("$f" -hide_banner -hwaccels 2>/dev/null | grep -q videotoolbox && echo yes || echo no)"
line "Flect app" "$(for a in "$HOME/Applications/Flect-for-Mac.app" /Applications/Flect-for-Mac.app dist/Flect-for-Mac.app; do [ -d "$a" ] && { echo "$a"; break; }; done)"
line "Quick Actions installed" "$(ls "$HOME/Library/Services" 2>/dev/null | grep -c '^Flect .*\.workflow$')  (the app has 42)"

echo
echo "== 2. Test files in $TEST_DIR"
FFMPEG="$(tool ffmpeg)"
if [ -z "$FFMPEG" ]; then
  echo "skipped: ffmpeg is needed to make the sample video and image (brew install ffmpeg-full)"
else
  mkdir -p "$TEST_DIR" && cd "$TEST_DIR" || exit 1
  [ -f sample.mp4 ] || "$FFMPEG" -loglevel error -y -f lavfi -i testsrc=duration=10:size=1280x720:rate=30 -f lavfi -i sine=frequency=440:duration=10 -c:v libx264 -c:a aac -pix_fmt yuv420p -shortest sample.mp4
  [ -f photo.png ] || "$FFMPEG" -loglevel error -y -f lavfi -i testsrc=size=1600x900:rate=1 -frames:v 1 photo.png
  [ -f sample.txt ] || printf 'こんにちは\r\nFlect test\r\n' > sample.txt
  [ -f sample.json ] || printf '{"b":1,"a":[3,2,1]}' > sample.json
  mkdir -p dup empty1/empty2 && [ -f dup/a.txt ] || { echo hello > dup/a.txt; cp dup/a.txt dup/b.txt; }
  make_pdf() { # name pages: one marked page per form feed
    out="$1"; n="$2"; txt="$(mktemp)"; i=1
    while [ "$i" -le "$n" ]; do printf '=== PAGE %s of %s ===\n\nFlect check page %s\n\f' "$i" "$n" "$i"; i=$((i + 1)); done > "$txt"
    /usr/sbin/cupsfilter -i text/plain -m application/pdf "$txt" > "$out" 2>/dev/null || rm -f "$out"
    rm -f "$txt"
  }
  [ -f five-pages.pdf ] || make_pdf five-pages.pdf 5
  [ -f two-pages.pdf ] || make_pdf two-pages.pdf 2
  [ -f five-pages.pdf ] && cp -n five-pages.pdf original-five-pages.pdf   # an untouched copy to compare against
  ls -1 "$TEST_DIR"
  if [ -f five-pages.pdf ] && command -v swift >/dev/null 2>&1; then
    echo; echo "PDF check (should be 5 pages, each marked 'PAGE n of 5'):"
    swift "$ROOT/scripts/mac-verify/pdf-info.swift" "$TEST_DIR/five-pages.pdf" 2>&1 | head -8
  elif [ ! -f five-pages.pdf ]; then
    echo "note: could not make the sample PDFs; use any PDF with 5 or more pages and name it five-pages.pdf"
  fi
  cd "$ROOT" || exit 1
fi

echo
echo "== 3. Web download, end to end (real engine, real yt-dlp, a local server; nothing leaves this Mac)"
if [ -n "$(tool yt-dlp)" ] && [ -n "$(tool ffprobe)" ]; then
  if [ -n "$ENGINE" ]; then python3 scripts/mac-verify/download_e2e.py "$ENGINE"; else python3 scripts/mac-verify/download_e2e.py; fi
  rc=$?
  case "$rc" in
    0) echo "-> download check: ALL PASSED" ;;
    2) echo "-> download check: could not run (see above)" ;;
    *) echo "-> download check: FAILED (paste the lines marked FAIL into the report)" ;;
  esac
else
  echo "skipped: needs yt-dlp and ffmpeg/ffprobe in /opt/homebrew/bin  ->  brew install yt-dlp ffmpeg-full"
fi

echo
echo "== Next: open docs/mac-port/19-mac-verification-checklist.ja.md and go through sections A to F."
echo "   Remove the test files afterwards with: scripts/mac-verify.sh --clean"
