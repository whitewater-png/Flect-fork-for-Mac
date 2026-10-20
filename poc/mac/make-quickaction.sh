#!/bin/bash
# Generates a minimal "Flect PoC" Quick Action WITHOUT Automator, to rule out hand-made mistakes.
# UNTESTED on Linux (cannot run Automator/Finder here) - run on your Mac, then:
#   killall Finder    and right-click a file > Quick Actions
# Remove with:  rm -r ~/Library/Services/"Flect PoC.workflow"
set -eu
NAME="Flect PoC"
DEST="$HOME/Library/Services/$NAME.workflow"
mkdir -p "$DEST/Contents"
UUID1=$(uuidgen); UUID2=$(uuidgen); UUID3=$(uuidgen)

cat > "$DEST/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>NSServices</key><array><dict>
    <key>NSBackgroundColorName</key><string>background</string>
    <key>NSIconName</key><string>NSActionTemplate</string>
    <key>NSMenuItem</key><dict><key>default</key><string>$NAME</string></dict>
    <key>NSMessage</key><string>runWorkflowAsService</string>
    <key>NSRequiredContext</key><dict><key>NSApplicationIdentifier</key><string>com.apple.finder</string></dict>
    <key>NSSendFileTypes</key><array><string>public.item</string></array>
  </dict></array>
</dict></plist>
PLIST

# The shell body: stage the selection in a private temp file and open it in TextEdit.
cat > "$DEST/Contents/document.wflow" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>AMApplicationBuild</key><string>523</string>
  <key>AMApplicationVersion</key><string>2.10</string>
  <key>AMDocumentVersion</key><string>2</string>
  <key>actions</key><array><dict><key>action</key><dict>
    <key>AMAccepts</key><dict><key>Container</key><string>List</string><key>Optional</key><true/><key>Types</key><array><string>com.apple.cocoa.string</string></array></dict>
    <key>AMActionVersion</key><string>2.0.3</string>
    <key>AMApplication</key><array><string>Automator</string></array>
    <key>AMParameterProperties</key><dict>
      <key>COMMAND_STRING</key><dict/><key>CheckedForUserDefaultShell</key><dict/>
      <key>inputMethod</key><dict/><key>shell</key><dict/><key>source</key><dict/>
    </dict>
    <key>AMProvides</key><dict><key>Container</key><string>List</string><key>Types</key><array><string>com.apple.cocoa.string</string></array></dict>
    <key>ActionBundlePath</key><string>/System/Library/Automator/Run Shell Script.action</string>
    <key>ActionName</key><string>Run Shell Script</string>
    <key>ActionParameters</key><dict>
      <key>COMMAND_STRING</key><string>f="\$(mktemp -t flect-sel)"; chmod 600 "\$f"
for p in "\$@"; do printf '%s\n' "\$p" &gt;&gt; "\$f"; done
open -n -a TextEdit "\$f"</string>
      <key>CheckedForUserDefaultShell</key><true/>
      <key>inputMethod</key><integer>1</integer>
      <key>shell</key><string>/bin/bash</string>
      <key>source</key><string></string>
    </dict>
    <key>BundleIdentifier</key><string>com.apple.RunShellScript</string>
    <key>CFBundleVersion</key><string>2.0.3</string>
    <key>CanShowSelectedItemsWhenRun</key><false/>
    <key>CanShowWhenRun</key><true/>
    <key>Category</key><array><string>AMCategoryUtilities</string></array>
    <key>Class Name</key><string>RunShellScriptAction</string>
    <key>InputUUID</key><string>$UUID1</string>
    <key>OutputUUID</key><string>$UUID2</string>
    <key>UUID</key><string>$UUID3</string>
    <key>UnlocalizedApplications</key><array><string>Automator</string></array>
    <key>arguments</key><dict/>
    <key>isViewVisible</key><integer>1</integer>
  </dict></dict></array>
  <key>connectors</key><dict/>
  <key>workflowMetaData</key><dict>
    <key>serviceApplicationBundleID</key><string>com.apple.finder</string>
    <key>serviceInputTypeIdentifier</key><string>com.apple.Automator.fileSystemObject</string>
    <key>serviceOutputTypeIdentifier</key><string>com.apple.Automator.nothing</string>
    <key>serviceProcessesInput</key><integer>0</integer>
    <key>workflowTypeIdentifier</key><string>com.apple.Automator.servicesMenu</string>
  </dict>
</dict></plist>
PLIST

plutil -lint "$DEST/Contents/Info.plist" "$DEST/Contents/document.wflow"
/System/Library/CoreServices/pbs -flush 2>/dev/null || true
echo "Created: $DEST"
echo "Next: killall Finder ; then right-click a file in Finder > Quick Actions > '$NAME'."
echo "If it still does not appear: System Settings > General > Login Items & Extensions > Finder, and enable it there."
