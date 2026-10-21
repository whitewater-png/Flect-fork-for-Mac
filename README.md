# Flect-for-Mac

**Right-click files in Finder to convert, compress, resize and more.** A macOS port of [Flect](https://github.com/Konayukiw/Flect) (Windows), rebuilt with a Rust processing core and a Swift front end.

> **Status: 0.2.0.** Verified on a MacBook (macOS 26, Apple Silicon), see [`docs/mac-port/`](docs/mac-port/). Builds are ad-hoc signed, **not notarized**. See [`CHANGELOG.md`](CHANGELOG.md) and the release notes for known limitations.

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
- `scripts/setup-deps.sh` checks for the tools Flect calls but does not bundle (ffmpeg-full, 7-Zip, optional Java/CFR and a subtitle model) and installs them with Homebrew after asking; `--check` only reports. The app lists what is missing under **Required tools** in its settings, with a command to copy. Or by hand: `brew install ffmpeg-full sevenzip`

## Download (ad-hoc signed build)

Releases contain `Flect-for-Mac-macos-arm64.dmg` (recommended) and `Flect-for-Mac-macos-arm64.zip` (Apple Silicon, macOS 14+), each with a `.sha256`. The app is **not notarized**, so macOS warns on first launch. The steps below are also drawn inside the disk image.

**From the disk image (easiest)**

1. Open the `.dmg` and drag **Flect-for-Mac** onto **Applications**.
2. Open Flect-for-Mac. If macOS says it cannot verify the developer, allow it once:
   - macOS 15 or later: try to open it, close the message (do not click "Move to Trash"), then **System Settings → Privacy & Security → Open Anyway** (it asks for your password and is available for about an hour after the attempt).
   - macOS 14: right-click the app → **Open** → **Open**.
3. The app starts on the **Getting started** tab: **Install the Quick Actions**, then **Open the Finder settings** and switch the Flect items on (details below). The **Required tools** tab lists the extra tools some items need (`brew install ffmpeg-full sevenzip`).

**From the zip, in Terminal**

```bash
shasum -a 256 -c Flect-for-Mac-macos-arm64.zip.sha256     # verify the download
unzip Flect-for-Mac-macos-arm64.zip && mv Flect-for-Mac.app ~/Applications/
xattr -dr com.apple.quarantine ~/Applications/Flect-for-Mac.app   # skips the macOS warning
~/Applications/Flect-for-Mac.app/Contents/Resources/flect-tools --out ~/Library/Services   # or use the button in the app
killall Finder
```
Then turn on the Quick Actions (next section). If you would rather not trust a binary, build it yourself (below). You also need `brew install ffmpeg-full sevenzip`.

## Build and install

```bash
git clone https://github.com/whitewater-png/Flect-fork-for-Mac.git
cd Flect-fork-for-Mac
scripts/build.sh --install     # builds, ad-hoc signs, installs ~/Applications/Flect-for-Mac.app and the Quick Actions
killall Finder
```

## Turn on the Quick Actions (first time only)

macOS leaves new Quick Actions **off**, so they do not show up until you turn them on:

1. Click **Open the Finder settings** on the app's **Getting started** tab, or open **System Settings → General → Login Items & Extensions** yourself, scroll to **Extensions**, and click the (i) button next to **Finder**.
2. Switch on every **Flect: …** item (38 of them). You can leave the others as they are.
3. If a Flect item is still missing, run `killall Finder`.

Then right-click a file in Finder → **Quick Actions** → Flect. Open the app itself to change settings.

## Other ways to start, and notifications (optional)

Besides the Quick Actions, Flect can be started in these ways. Two of them need a switch in System Settings, because macOS leaves anything new **off**:

- **Drag and drop / ⌘O:** drop files or folders on the Flect icon in the Dock (or on Flect.app), or choose **File → Open…** (⌘O) in the app. Both open a window where you pick what to do. Nothing needs to be switched on.
- **Services → "Process with Flect…":** select files in Finder and choose **Finder → Services → Process with Flect…**. It does not appear until you switch it on: open **System Settings → Keyboard → Keyboard Shortcuts… → Services**, find **Process with Flect…** under **Files and Folders**, and tick it. (The **Flect: …** Quick Actions are also listed in the Services menu, so they work from there even while they are off in the Finder extension settings.)
- **Notification when a run finishes:** open **System Settings → Notifications**, choose **Flect** and switch on **Allow notifications**. Until then Flect stays silent. Clicking the notification opens that run under **File → Recent Runs…**.

If **Process with Flect…** is still missing after you tick it, quit and reopen Finder (`killall Finder`) or log out and in again.

## Updates

Flect does not update itself. To learn about new versions:

- **In the app:** settings → **Getting started** → **4. Updates** → **Check for updates**. It asks GitHub once (see below) and, if there is a newer release, shows a banner and a button that opens the download page. You can also let it check once a day when the settings window opens (off by default).
- **From GitHub:** on the repository page choose **Watch → Custom → Releases** to get a notification (and an email, if you enabled it) for each new release. Or subscribe to the feed `https://github.com/whitewater-png/Flect-fork-for-Mac/releases.atom` in a feed reader.

Then install the new version the same way as the first (drag the new app onto Applications and replace the old one); your settings are kept. If the **Getting started** tab says the Quick Actions are from an older version, click **Install again** there (needed once when updating from 0.2.0 or earlier: older Quick Actions no longer start Flect with more than one item selected).

## Reporting problems

Bugs and questions: open an issue (**Issues** tab) with your macOS version, the Flect version (Finder → Get Info on the app), what you did and what happened. Please do not attach files you would not want to be public. Security problems: report them privately, see [SECURITY.md](SECURITY.md).

## Security design

- The Swift app does the UI and OS work; all file processing runs in a separate Rust process (`flect-engine`, `#![forbid(unsafe_code)]`) that talks JSON Lines over stdio. One process per run.
- External tools run with argument arrays only (no shell), from fixed directories, with ffmpeg's protocol whitelist restricted to `file,pipe`.
- Archive extraction rejects absolute and `..` paths, links, and decompression bombs.
- Outputs are created exclusively; existing files are never overwritten. Duplicate deletion is confirmed by byte comparison.
- A configured output folder (Settings > Output) that is a symbolic link, or lies behind one, is refused with an error (`msg.outputFolderLink`) instead of being followed; `~`, absolute and relative paths to real folders work as before.
- Launch arguments are untrusted: `--task` may only name a menu id or `preset:<UUID>` (never engine parameters, so deleting tasks always start with their preview), and `--input` is read and removed only if it is a Quick Action's own selection file (a plain file of yours named `flect-sel.*` directly in your temporary folder, under 64 MiB). The selection is NUL-separated, so a line break in a file name cannot add an item.
- JAR decompilation uses a pinned (SHA-256) CFR jar and only ever runs under `sandbox-exec` (no network, writes only to its staging and private temporary folder, no other programs). If the sandbox cannot be set up, nothing is decompiled (`msg.sandboxFailed`); there is no unsandboxed fallback.
- Network: the app makes **no connection of its own** except the update check you start (or switch on): one HTTPS request to `api.github.com` that sends nothing about you or your Mac, and whose answer is accepted only if it is a plain `vX.Y.Z` release of this repository. Nothing is downloaded or installed automatically.
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
