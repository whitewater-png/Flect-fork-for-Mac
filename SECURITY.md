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

## Deleting tasks: what is and is not guaranteed

Remove Duplicate and Remove Empty only remove what was checked, but they run while other programs
may change the same files. What Flect does:

- Permanent deletion goes through handles of verified folders, never by path: each folder is
  reached from the selected folder without following links and must be the one scanned (device
  and inode); each file or folder is renamed to a fresh temporary name (never replacing anything),
  checked again under that name, and only then removed. For a duplicate, both copies are compared
  byte for byte right before, and the copy that stays is checked once more last of all.
- In Trash mode the engine does the same checks and the comparison, then hands one item at a time
  to the app with what it verified (device, inode, size, modification and change time of the item,
  and of the copy that stays), and checks the next only after the app has dealt with it. The app
  moves an item only while all of that still matches exactly; it does not compare contents again.

Known limits:

- A change that keeps device, inode, size and both times identical cannot be seen. On FAT and
  exFAT, modification times have a 2-second resolution, so a same-size rewrite within the
  same 2 seconds, between the engine's check and the app's move to the Trash, is not detected.
  Moving to the Trash can be undone.
- Between the last check under the temporary name and the removal, only the temporary name is
  used; a process that lists the folder and renames that exact name in that moment is not
  defended against.
- On volumes whose rename does not support the no-replace flag (some FAT, exFAT and network
  volumes), the temporary rename checks that the random name is unused and then renames.
