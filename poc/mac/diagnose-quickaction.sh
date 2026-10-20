#!/bin/bash
# Read-only diagnosis for "my Quick Action does not show in Finder". UNTESTED on Linux - run on your Mac.
# Paste the whole output back. Changes nothing.
set -u
echo "== macOS"; sw_vers
echo; echo "== ~/Library/Services"
ls -la ~/Library/Services 2>&1
for w in ~/Library/Services/*.workflow; do
  [ -e "$w" ] || continue
  echo; echo "--- $w"
  echo "[Info.plist]";     plutil -p "$w/Contents/Info.plist" 2>&1 | head -40
  echo "[workflowMetaData]"; plutil -extract workflowMetaData xml1 -o - "$w/Contents/document.wflow" 2>&1 | head -30
  echo "[lint]"; plutil -lint "$w/Contents/Info.plist" "$w/Contents/document.wflow" 2>&1
  echo "[xattr]"; xattr -l "$w" 2>&1 | head -5
done
echo; echo "== Services registered (pbs)"
/System/Library/CoreServices/pbs -dump_pboard 2>/dev/null | grep -i -B1 -A8 "flect\|poc" | head -60 || true
echo; echo "== Finder-related extension state (pluginkit)"
pluginkit -mAvv 2>/dev/null | grep -i -E "quick|workflow|Automator" | head -20
echo; echo "== Is a Finder restart / pbs refresh needed? (not done by this script)"
echo "Try:  /System/Library/CoreServices/pbs -flush   then   killall Finder"
echo; echo "== Manual check (please answer in your reply)"
cat <<'TXT'
1. System Settings > General > Login Items & Extensions > (scroll) "Finder" under Extensions:
   is there a list of Quick Actions, and is yours present/checked?
2. Right-click a file in Finder: is there a "Quick Actions" item at all? Is "Services" present at the bottom?
3. Finder menu bar > Finder > Services: is your item listed there?
TXT
