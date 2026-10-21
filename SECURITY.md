# Security Policy

## Reporting a vulnerability

Please do **not** open a public issue for a security problem. Use GitHub's
**Security → Report a vulnerability** (private vulnerability reporting) on this repository.
Include the file or command that triggers it and your macOS version. Reports are handled on a best-effort basis.

## Supported versions

Only the latest release (and the default branch) receives fixes.

## Scope

Flect-for-Mac processes files you select in Finder, so the interesting bugs are
those reachable through a crafted file:

- archive extraction escaping its destination (path traversal, links), or exhausting disk/CPU (decompression bombs)
- data-format conversion (XML/CSV/JSON) reading local files or exhausting memory
- a request to the engine that makes it read or write outside the selected items
- unsafe use of the external tools (ffmpeg, 7-Zip, Java)

## Design principles

See `docs/mac-port/02-requirements.ja.md` section 4.3 (SEC-1 ... SEC-11).
Notably: the processing core is Rust with `unsafe` forbidden, external tools are
separate processes started with argument vectors (never a shell), originals are
never modified, and results are written without overwriting anything.
