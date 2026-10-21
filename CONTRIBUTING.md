# Contributing

Flect-for-Mac is a Rust core + Swift front end. The original Windows sources live in `legacy/windows/` and define the expected behavior.

## Build and test

```bash
cd core && cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test
cd ../app && swift build && swift test        # macOS only
cd .. && scripts/build.sh                     # .app bundle in dist/
```

CI (`.github/workflows/ci.yml`) runs all of this plus `cargo deny` on every push.

## Rules that keep the project safe

1. **Never overwrite or delete the user's files** except through the documented paths (exclusive output creation; Trash via the app).
2. **No shell.** Start tools with argument arrays from fixed directories; keep ffmpeg's `-protocol_whitelist file,pipe`.
3. **No `unsafe`** in `core/` (`#![forbid(unsafe_code)]`). New dependencies must pass `cargo deny check` and be added to `NOTICES.md`.
4. **Engine messages are keys + args**; the Swift side localizes them. Add new keys to `app/Sources/FlectCore/Resources/strings.{en,ja,zh}.json` (a test fails if a key has no translation).
5. Archive and media code must keep their size, count and ratio limits.

## Reporting problems

Open an issue for bugs and questions (macOS version, Flect version, steps, what happened). Report security problems privately: see `SECURITY.md`.

## Pull requests

Keep changes focused, add a test where the behavior is testable without a Mac, and describe what you verified on real hardware. Licensing: contributions are accepted under Apache-2.0.
