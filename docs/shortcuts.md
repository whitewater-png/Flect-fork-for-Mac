# Shortcuts (Shortcuts.app)

Two ways to use Flect from Shortcuts. Read the status section first: only the second one is known to work
with a build made by `scripts/build.sh` on this project's toolchain.

## Status (what was and was not verified)

| Item | Status |
| --- | --- |
| App Intents code (`app/Sources/FlectApp/Intents.swift`) compiles against the macOS 26.5 SDK with deployment target macOS 14 | Verified: `swift build` succeeds |
| Task selection, option checks, file checks, never-overwrite output, "only reported outputs are returned" (`app/Sources/FlectCore/ShortcutsPlan.swift`) | Verified with a throwaway harness against the real `flect-engine` and its real menu catalog (strip metadata twice gives `e_clean.png`, `e_clean (2).png`; convert; resize; refusals for folders, HEIC, mixed file kinds, junk options). The harness is not part of the repository, and XCTest is unavailable with only the Command Line Tools, so `swift test` was **not** run |
| Flect's actions appear in Shortcuts.app and run | **Not verified, and not expected to work yet.** Shortcuts discovers actions from `Contents/Resources/Metadata.appintents` inside the app. That folder is produced by `appintentsmetadataprocessor`, a tool of full Xcode. This machine has only the Command Line Tools, where the tool does not exist (`xcrun --find appintentsmetadataprocessor` fails). `swift build` and the manual bundling in `scripts/build.sh` do not create it |
| `flect` Run Shell Script recipe below | Verified from a shell (output and exit codes). Not run inside Shortcuts.app itself |

Not faked: no `Metadata.appintents` is written by hand (its format is private and changes between Xcode
versions), and `build.sh` is unchanged. Until a build with that metadata exists, treat the App Intents as
"implemented, compiles, not observed in Shortcuts".

### What is needed to turn the App Intents on

A build made with full Xcode (the Swift compiler writes constant-value files, and Xcode's build step runs
`appintentsmetadataprocessor` over them and the SDK's `AppIntents` metadata), for example an Xcode project or
`xcodebuild` workspace that includes the `app/Sources` files and then the same bundling and ad-hoc signing as
`scripts/build.sh`. Then check, on that build:

1. `ls Flect-for-Mac.app/Contents/Resources/Metadata.appintents` exists (after signing it must not break
   `codesign --verify --deep --strict`).
2. Open Shortcuts.app, search for "Flect" in the action list. The four actions below should be listed.
3. Run each on a test image and confirm the results are new files next to the originals.

Only after those three are observed should this document say the integration works.

## The actions (when the metadata exists)

| Action | Does | Engine task |
| --- | --- | --- |
| Run a Flect Task on Files | Pick a task from a list built from the engine's menu, give files, get the created files back | Any catalog leaf that needs no dialog (about 60: presets, conversions, rotate, compress, strip, extract ...) |
| Resize Images with Flect | Percent (default 50) or width and/or height in pixels, optional keep-aspect | `image.resize` |
| Convert Images with Flect | PNG, JPEG, WebP, ICO, GIF, SVG | `image.convert` |
| Make Images Safe to Share with Flect | Makes safe copies: removes EXIF/GPS, comments, XMP/IPTC and other hidden data, then re-checks the result and fails if anything personal remains | `image.makeSafe` |

"Make safe to share" runs the engine's `image.makeSafe` task (the same as the menu entry of that name), which
verifies its output; it is not the plainer "Remove Metadata" (`image.strip`). It does not blur faces, redact
visible content or touch PDFs.

Not offered from Shortcuts, on purpose: folder tasks (they can ask to move things to the Trash), PDF tasks and
HEIC/background removal/OCR (they need the app's OS features and a window), tasks that need a dialog
("Custom...", Trim, Thumbnail).

### How they stay inside the app's rules

- The task list comes from `flect-engine export-menu`; a task is offered only if `Planner.resolve` turns it into
  a plain engine task. The task is re-resolved when the action runs, and must still be in the catalog and in an
  allowed part of it (image, video, audio, text, archive).
- Numbers (percent 1-1000, pixels 1-32768) are checked before a task string is built; no free text reaches the
  task string. File names never reach a shell: the engine is started directly as a child process.
- Files from Shortcuts are untrusted: they go through `EntryRequest` / `LaunchRequest.validate` (absolute path,
  length, NUL, exists, symlinks not followed), and every file must be of a kind the chosen task applies to
  (the same decision as the action picker). One bad file fails the whole run with a message.
- A file given as data only (no path) is written to a fresh private folder under the system temporary folder
  with a sanitised name, and results land beside it. That folder is left for the system to clear.
- Output follows the engine's rules: new names beside the original (or in the configured output folder),
  never overwriting. The action returns only paths the engine reported in its `outputs` event, after
  `RunRecorder` checks that each is a file created by this run and not an input or a folder above one.
- `needs: trash` requests are ignored. A run that does not succeed fails the action (files that were already
  created stay on disk).
- Intent runs are not added to the "Recent runs" list / Undo, to avoid two processes writing the history file.

## Fallback that works today: Run Shell Script with `flect`

Uses the bundled command-line tool (see `docs/cli.md`). In Shortcuts add "Run Shell Script", shell `/bin/zsh`,
"Input: Shortcut Input", "Pass Input: as arguments", and paste. Change `TASK` as needed (`flect list` shows the
ids); a task with options needs them on the run line, for example `run image.resize -o scale=50 --json -- "$@"`:

```zsh
FLECT="$HOME/Applications/Flect-for-Mac.app/Contents/Resources/flect"
TASK="image.strip"

events="$("$FLECT" run "$TASK" --json -- "$@")" || { echo "Flect failed" >&2; exit 1; }

# Print only the files the engine reports as created, one path per line.
printf '%s\n' "$events" | /usr/bin/osascript -l JavaScript -e '
ObjC.import("Foundation");
var data = $.NSFileHandle.fileHandleWithStandardInput.readDataToEndOfFile;
var text = ObjC.unwrap($.NSString.alloc.initWithDataEncoding(data, 4));
var out = [];
text.split("\n").filter(function (l) { return l.length; }).forEach(function (l) {
  var e = JSON.parse(l);
  if (e.event === "data" && e.name === "outputs") e.data.paths.forEach(function (p) { out.push(p); });
});
$.NSFileHandle.fileHandleWithStandardOutput.writeData($(out.join("\n") + "\n").dataUsingEncoding(4));
'
```

The output is one created path per line; a following "Get File" or "Split Text" (by New Lines) then "Get Files
at path" action can pick them up. Checked from a shell with `image.strip`: passing the same image twice, after
an earlier run had created `a_clean.png`, gave `a_clean (2).png` and `a_clean (3).png` (original and earlier
result untouched), and an unknown task printed "Flect failed" and exited 1. The same trust rules apply as for the CLI: the engine is the sibling of
`flect` inside the app bundle, never found through `PATH`, and nothing goes through a shell except the
arguments you pass to `flect` itself, quoted by `"$@"`. A Shortcut that passes file names starting with `-` is
safe because of the `--` before them.
