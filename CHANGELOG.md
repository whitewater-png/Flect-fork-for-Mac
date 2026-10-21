# Changelog

## Unreleased

- Share safely (images): **Privacy Check** shows what personal data a JPEG, PNG or WebP carries (GPS location, camera and lens serial numbers, owner/author, timestamps, software, embedded thumbnails, XMP/IPTC, comments, hidden blocks) and changes nothing. **Make Safe to Share** writes a new `_safe` copy without it (lossless for JPEG/PNG/WebP unless the rotation has to be applied; optionally with a maximum long edge via **Make Safe to Share (Resize)**). The copy is re-checked before it is written and the original is never touched or overwritten. Both appear as Finder Quick Actions. HEIC, PDF and video are not covered yet.
- Duplicate removal now shows a preview first: for each group the copy that stays and the copies that would go. Nothing is moved until you confirm (Trash by default; Cancel is the default button).
- Output folders: **Settings → Output** sets a folder per feature (images, video, audio, text files, PDF, OCR, extracted archives, compressed folders, analysis reports). Empty means beside the original, as before. A relative path such as `./Compressed` starts from the original's folder; absolute paths and `~` work; the folder is created when missing. Existing files are never overwritten (` (2)`, ` (3)` numbering is unchanged).
- Zip files made on Windows with Japanese or Chinese names (Shift_JIS / GBK, no UTF-8 flag) now extract with readable names instead of mojibake. All path, link and size checks still apply.

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
