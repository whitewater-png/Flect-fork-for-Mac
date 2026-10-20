#!/bin/bash
# P0 items 2,6: toolchain + VideoToolbox + display adapters. UNTESTED here - run on your Mac.
set -u
echo "== macOS / arch"; sw_vers -productVersion; uname -m
echo "== toolchain"; for c in xcodebuild swift cargo rustc ffmpeg ffprobe 7zz java python3; do printf '%-10s' $c; command -v $c || echo "-"; done
echo "== ffmpeg license / VideoToolbox"
if command -v ffmpeg >/dev/null; then
  ffmpeg -hide_banner -version | head -3
  ffmpeg -hide_banner -encoders 2>/dev/null | grep -E "videotoolbox" || echo "no videotoolbox encoders"
  for e in h264_videotoolbox hevc_videotoolbox; do
    ffmpeg -hide_banner -loglevel error -nostdin -f lavfi -i color=c=black:s=320x240:d=1 -c:v $e -f null - && echo "OK $e" || echo "NG $e"
  done
fi
echo "== GPU"; system_profiler SPDisplaysDataType | grep -E "Chipset|Cores|Metal" 
echo "== Finder Sync extensions (enabled list)"; pluginkit -mA -p com.apple.FinderSync 2>/dev/null || true
