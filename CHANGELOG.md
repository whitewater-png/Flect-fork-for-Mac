# Changelog

## Unreleased

- **Reinstall the Quick Actions after updating** (Getting started → **Install again**; the tab now says when the installed ones are outdated and the settings window opens on it). Quick Actions from 0.2.0 and earlier still work for a single selected item; with more than one item Flect now refuses them and says how to reinstall.
- Security: the selection file passed with `--input` is read and removed only when it is a Quick Action's own file (a plain file you own, named `flect-sel.*`, directly in your temporary folder, under 64 MiB), and it is removed with `unlink`. Before, `open -a Flect-for-Mac --args --task x --input <any path>` deleted that path, folders included, recursively.
- Security: `--task` from the launch arguments may only name a menu id (what the Quick Actions pass) or `preset:<UUID>`. Engine parameters such as `folder.removeDuplicate?mode=all`, `folder.removeEmpty?confirm=1` or `folder.rename?...&dry=0` are refused instead of deleting or renaming without the preview. Every entry point (Quick Action, Dock drop, Open…, Services, pinned items, Run again) starts Remove Duplicate, Remove Empty and Rename with their preview or dialog.
- Security: the Quick Actions now write the selection NUL-separated and the app splits on NUL only, without trimming, and requires every listed item to exist. Before, a file named `x⏎/Users/me` added `/Users/me` to the selection.

## 0.2.0 - share safely, presets, command line

- Share safely (images): **Privacy Check** shows what personal data a JPEG, PNG or WebP carries (GPS location, camera and lens serial numbers, owner/author, timestamps, software, embedded thumbnails, XMP/IPTC, comments, hidden blocks) and changes nothing. **Make Safe to Share** writes a new `_safe` copy without it (lossless for JPEG/PNG/WebP unless the rotation has to be applied; optionally with a maximum long edge via **Make Safe to Share (Resize)**). The copy is re-checked before it is written and the original is never touched or overwritten. Both appear as Finder Quick Actions. HEIC, PDF and video are not covered yet.
- Duplicate removal now shows a preview first: for each group the copy that stays and the copies that would go. Nothing is moved until you confirm (Trash by default; Cancel is the default button).
- Output folders: **Settings → Output** sets a folder per feature (images, video, audio, text files, PDF, OCR, extracted archives, compressed folders, analysis reports). Empty means beside the original, as before. A relative path such as `./Compressed` starts from the original's folder; absolute paths and `~` work; the folder is created when missing. Existing files are never overwritten (` (2)`, ` (3)` numbering is unchanged).
- Presets and chains: **Settings → Presets** saves named lists of 1 to 8 steps (for example "SNS 1080px / 2MB" = resize to 1080 px, then compress to 2 MB, or resize → convert to JPG → remove metadata). They appear in the action picker for matching files and run from Terminal with `flect run --preset NAME`. Intermediate files stay in a private temporary folder that is removed afterwards; only the final file is written next to the original (never overwriting), and Undo covers exactly that file. Cancel stops the chain.
- Action picker: pin up to 6 tasks or presets to the top (right-click → Pin to top), and **Run again** repeats the last operation on a new selection when it applies.
- Fixed: Text → Convert (to JSON, CSV, XML...) was answered "not available yet" by the engine.
- Zip files made on Windows with Japanese or Chinese names (Shift_JIS / GBK, no UTF-8 flag) now extract with readable names instead of mojibake. All path, link and size checks still apply.
- Command line: the app now bundles `flect` (`Flect-for-Mac.app/Contents/Resources/flect`) that runs the same tasks as the menu: `flect list`, `flect run <task> [-o key=value] [--json] <paths>`, `flect run --preset NAME`, `flect settings path`. Exit codes 0 / 1 / 2 / 130 (cancelled). Tasks that delete (**Remove Duplicate**, **Remove Empty**) only preview unless you pass `--apply` (and `--permanent` when the saved setting is permanent deletion). See `docs/cli.md`.
- More ways to start: drop files on the Dock icon or on Flect.app, **File → Open…** (⌘O), and the **Services** entry "Process with Flect…". All of them open the same action picker and go through the same checks as the Quick Actions. macOS leaves the Services entry and notifications **off** until you switch them on; see the README.
- Results and Undo: after a run the window lists the files it created, with **Show in Finder** and **Undo** (moves only the files that run created to the Trash, and only if they are unchanged). **File → Recent Runs…** (⌘Y) keeps the last 20 runs. A notification tells you when a run finishes (once you allow it in System Settings).
- Faster batches: many images or text files are processed side by side (4 workers by default; `jobs` option or `FLECT_JOBS` to change). Results are reported in input order and cancel still stops promptly. Video, audio, archives and folder tasks are unchanged.
- Shortcuts: App Intents for Shortcuts.app are included in the source (`Run a Flect Task on Files`, `Resize Images`, `Convert Images`, `Make Images Safe to Share`), but this build cannot register them with Shortcuts (that needs a build with full Xcode). Until then use the `flect` command in a **Run Shell Script** action; see `docs/shortcuts.md`.
- Fixed: the results header showed "1 file created" in English in a Japanese or Chinese UI when exactly one file was created.
- Security hardening found in review: output folders that are symbolic links are refused; the copy fallback when hard links are unavailable no longer leaves a truncated file under the final name; a closed output pipe stops a parallel batch; metadata hidden between the scans of a progressive JPEG is found and removed; control characters in file names are not printed raw by the CLI.

## 0.1.2 - update check

- Update check: **Getting started → Updates → Check for updates** asks GitHub's public API once (HTTPS to `api.github.com`, nothing about you is sent) and shows a banner plus a button to the download page when a newer release exists. An optional daily check when the settings window opens is off by default. Nothing is downloaded or installed automatically. The README also explains GitHub's **Watch → Custom → Releases** and the `releases.atom` feed.

## 0.1.1 - easier install

- Disk image (`Flect-for-Mac-macos-arm64.dmg`): drag the app onto Applications; the window shows the steps for macOS's first-launch warning (Japanese and English), and a text file repeats them. The zip stays available.
- The app starts on a **Getting started** tab until the Quick Actions are installed: a button installs them (no Terminal needed) and another opens System Settings where they are switched on.
- The app's version is shown without the tag's leading "v" (0.1.1, not v0.1.1).
- Issues are enabled for bugs and questions (README, CONTRIBUTING).

## 0.1.0 - first release

A macOS port of [Flect](https://github.com/Konayukiw/Flect) (Windows): Rust processing core, Swift app, Finder Quick Actions.

- All 87 menu items of the Windows version: images, video/audio, PDF, text/JSON, folders, archives, JAR decompile, OCR.
- Subtitles through ffmpeg's `whisper` filter (SHA-256-pinned models, no Python).
- Background removal with the macOS Vision framework (falls back to a colour key).
- VideoToolbox hardware encoding for video.
- Settings window (general, folder, image, video, text, OCR, menu items) and a "Required tools" tab.
- `scripts/setup-deps.sh` and the app tell you which external tools are missing and how to install them.
- Ad-hoc signed build on GitHub Releases; not notarized.
- Verified on a MacBook (macOS 26, Apple Silicon) in two rounds; see `docs/mac-port/`.

Known limitations: see `docs/release/v0.1.0.md`.
