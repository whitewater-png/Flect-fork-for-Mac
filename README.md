# Flect-for-Mac

**Right-click files in Finder to convert, compress, resize and more.** A macOS port of [Flect](https://github.com/Konayukiw/Flect) (Windows), rebuilt with a Rust processing core and a Swift front end.

> **Status: pre-release.** Everything compiles and passes CI on macOS 14 (Apple Silicon), but the Finder integration and OS-specific features still need verification on real hardware. See [`docs/mac-port/11-mac-verification-guide.ja.md`](docs/mac-port/11-mac-verification-guide.ja.md) (Japanese). No signed or notarized builds exist yet - you build it yourself.

[日本語](README.Ja.md) / English

## What it does

Your originals are never touched: every task writes a new file next to the source (`name (2).ext` if the name is taken). Deletions go to the Trash by default.

| Area | Tasks |
| --- | --- |
| Images | resize (%, pixels), compress to a target size, rotate, convert (PNG, JPG, WEBP, HEIC, ICO, GIF, SVG), OCR, strip metadata, color-key background |
| Video / audio | resize, trim, thumbnail, compress (incl. Discord size), rotate, convert, extract audio (VideoToolbox optional) |
| PDF | preview, merge, to PNG / JPG / text |
| Text / JSON | convert formats, change encoding or line endings, pretty-print, sort keys |
| Folders | find duplicates (byte-verified), remove empty folders, tree, analyze, bulk rename, compress |
| Archives / code | safe extraction (ZIP, 7z, tar...), JAR decompile (CFR) |

Subtitles (speech to SRT/VTT/TXT) use the `whisper` filter of a whisper-enabled ffmpeg (e.g. Homebrew `ffmpeg-full`) plus a model installed with `scripts/fetch-whisper-model.sh`. Not implemented yet: AI background removal.

## Requirements

- macOS 14 or later, Apple Silicon
- Xcode Command Line Tools (Swift 5.9+), [Rust](https://rustup.rs)
- `scripts/setup-deps.sh` checks for the tools Flect calls but does not bundle (ffmpeg-full, 7-Zip, optional Java/CFR and a subtitle model) and installs them with Homebrew after asking; `--check` only reports. Or by hand: `brew install ffmpeg-full sevenzip`

## Download (ad-hoc signed build)

Releases contain `Flect-for-Mac-macos-arm64.zip` (Apple Silicon, macOS 14+) and its `.sha256`. The app is **not notarized**, so macOS will warn on first launch:

```bash
shasum -a 256 -c Flect-for-Mac-macos-arm64.zip.sha256     # verify the download
unzip Flect-for-Mac-macos-arm64.zip && mv Flect-for-Mac.app ~/Applications/
xattr -dr com.apple.quarantine ~/Applications/Flect-for-Mac.app
~/Applications/Flect-for-Mac.app/Contents/Resources/flect-tools --out ~/Library/Services   # install the Quick Actions
killall Finder
```
If you would rather not trust a binary, build it yourself (below). You also need `brew install ffmpeg-full sevenzip`.

## Build and install

```bash
git clone https://github.com/whitewater-png/Flect-fork-for-Mac.git
cd Flect-fork-for-Mac
scripts/build.sh --install     # builds, ad-hoc signs, installs ~/Applications/Flect-for-Mac.app and the Quick Actions
killall Finder
```

Then right-click a file in Finder → **Quick Actions** → Flect. Open the app itself to change settings.

## Security design

- The Swift app does the UI and OS work; all file processing runs in a separate Rust process (`flect-engine`, `#![forbid(unsafe_code)]`) that talks JSON Lines over stdio. One process per run.
- External tools run with argument arrays only (no shell), from fixed directories, with ffmpeg's protocol whitelist restricted to `file,pipe`.
- Archive extraction rejects absolute and `..` paths, links, and decompression bombs.
- Outputs are created exclusively; existing files are never overwritten. Duplicate deletion is confirmed by byte comparison.
- JAR decompilation uses a pinned (SHA-256) CFR jar under `sandbox-exec` when available.
- See [`SECURITY.md`](SECURITY.md) and [`docs/mac-port/02-requirements.ja.md`](docs/mac-port/02-requirements.ja.md).

## Repository layout

| Path | Contents |
| --- | --- |
| `core/` | Rust workspace: `flect-core` (processing), `flect-engine` (stdio engine), `flect-tools` (Quick Action generator) |
| `app/` | Swift package: `FlectCore` (testable logic), `FlectApp` (AppKit/SwiftUI app) |
| `scripts/` | `build.sh`, `fetch-cfr.sh` |
| `docs/mac-port/` | Design, protocol, parity table, verification guide (Japanese) |
| `legacy/windows/` | Original Windows sources, kept as the behavior reference |

## License

Apache License 2.0 - see [`LICENSE`](LICENSE), [`NOTICE`](NOTICE) and [`NOTICES.md`](NOTICES.md). Original work © Konayukiw.
