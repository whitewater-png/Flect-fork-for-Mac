#!/bin/bash
# Check for (and, with your confirmation, install) the tools Flect-for-Mac calls but does not bundle.
#   scripts/setup-deps.sh            interactive: asks before each install
#   scripts/setup-deps.sh --check    only report what is missing (changes nothing)
#   scripts/setup-deps.sh --yes      install everything without asking (still only the fixed commands below)
#   scripts/setup-deps.sh --whisper small   also install that subtitle model (tiny|base|small|medium|large-v3|turbo)
#
# What it can run, and nothing else:
#   brew install ffmpeg-full | sevenzip | openjdk      (Homebrew; no sudo)
#   fetch-cfr.sh                                        (pinned SHA-256; next to this script)
#   fetch-whisper-model.sh <model>                      (pinned SHA-256; next to this script)
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"

mode=ask; whisper=""
while [ $# -gt 0 ]; do
  case "$1" in
    --check) mode=check ;;
    --yes) mode=yes ;;
    --whisper) shift; whisper="${1:-small}" ;;
    -h|--help) sed -n 2,12p "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done

data="${FLECT_HOME:-$HOME/Library/Application Support/Flect-for-Mac}"
BREW=""; for b in /opt/homebrew/bin/brew /usr/local/bin/brew; do [ -x "$b" ] && BREW="$b" && break; done
BIN=""; for d in /opt/homebrew/bin /usr/local/bin; do [ -d "$d" ] && BIN="$BIN $d"; done

find_tool() { for d in $BIN; do [ -x "$d/$1" ] && { echo "$d/$1"; return 0; }; done; return 1; }

have_ffmpeg=no; have_whisper=no; have_7z=no; have_java=no; have_cfr=no
if t="$(find_tool ffmpeg)"; then have_ffmpeg=yes; "$t" -hide_banner -filters 2>/dev/null | awk '$2=="whisper"{f=1} END{exit !f}' && have_whisper=yes; fi
find_tool 7zz >/dev/null && have_7z=yes
have_ytdlp=no; find_tool yt-dlp >/dev/null && have_ytdlp=yes
if [ -x /usr/libexec/java_home ] && /usr/libexec/java_home >/dev/null 2>&1; then have_java=yes
elif [ -d /opt/homebrew/opt/openjdk ] || [ -d /usr/local/opt/openjdk ]; then have_java=yes; fi
[ -f "$data/deps/cfr.jar" ] && have_cfr=yes

row() { printf '%-34s %-9s %s\n' "$1" "$2" "$3"; }
echo "Flect-for-Mac dependencies"
row "Tool" "Status" "Needed for"
row "ffmpeg / ffprobe" "$have_ffmpeg" "video and audio tasks"
row "ffmpeg with the whisper filter" "$have_whisper" "subtitles (Homebrew: ffmpeg-full)"
row "7-Zip (7zz)" "$have_7z" "7z archives"
row "Java runtime" "$have_java" "JAR decompile (optional)"
row "CFR decompiler (pinned)" "$have_cfr" "JAR decompile (optional)"
row "yt-dlp" "$have_ytdlp" "web download (optional, off by default; never installed by this script)"
[ -n "$whisper" ] && row "Whisper model: $whisper" "requested" "subtitles"
[ "$have_ytdlp" = no ] && echo "Web download is optional: if you turn it on in Settings, install yt-dlp yourself with: brew install yt-dlp"
echo

if [ "$mode" = check ]; then
  [ "$have_ffmpeg" = yes ] && [ "$have_7z" = yes ]; exit $?
fi

confirm() { # $1 = question
  [ "$mode" = yes ] && return 0
  read -r -p "$1 [y/N] " a; [ "$a" = y ] || [ "$a" = Y ]
}
brew_has() { [ -n "$BREW" ] && "$BREW" list "$1" >/dev/null 2>&1; }
brew_install() { # $1 = formula, $2 = why
  if [ -z "$BREW" ]; then echo "Homebrew is not installed (https://brew.sh). Install it first, then run this again."; return 1; fi
  confirm "Install '$1' with Homebrew ($2)?" || { echo "skipped $1"; return 0; }
  "$BREW" install "$1" || echo "brew install $1 failed; see the message above."
}

if [ "$have_ffmpeg" = no ] || [ "$have_whisper" = no ]; then
  if [ "$have_ffmpeg" = yes ] && brew_has ffmpeg && ! brew_has ffmpeg-full; then
    echo "Your ffmpeg has no whisper filter. Subtitles need ffmpeg-full, which conflicts with the 'ffmpeg' formula:"
    echo "  brew unlink ffmpeg && brew install ffmpeg-full      (this script will not unlink anything for you)"
  else
    brew_install ffmpeg-full "video/audio and subtitles"
  fi
fi
[ "$have_7z" = no ] && brew_install sevenzip "7z archives"
[ "$have_java" = no ] && brew_install openjdk "JAR decompile (optional)"
if [ "$have_cfr" = no ]; then
  confirm "Download the pinned CFR decompiler (verified by SHA-256)?" && "$HERE/fetch-cfr.sh"
fi
if [ -n "$whisper" ]; then
  "$HERE/fetch-whisper-model.sh" "$whisper"
elif [ "$have_whisper" = yes ] && [ ! -d "$data/deps/whisper" ]; then
  confirm "Download the 'small' subtitle model (about 480 MB, verified by SHA-256)?" && "$HERE/fetch-whisper-model.sh" small
fi
echo; echo "Done. Run 'scripts/setup-deps.sh --check' to see the result."
