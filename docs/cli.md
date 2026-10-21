# Command-line use (`flect`)

`flect` runs the same processing as the app from a terminal or a script. It does no file
processing itself: it starts the sibling `flect-engine` process and talks the JSON-Lines
protocol to it (see `docs/mac-port/08-engine-protocol.ja.md`), so the security design is the
same as the app's (separate process, no shell, `forbid(unsafe_code)`). The engine is looked up
only next to the `flect` executable (symlinks are resolved), never via `PATH`.

## Install

`scripts/build.sh` puts `flect` next to the engine inside the app bundle
(`Flect-for-Mac.app/Contents/Resources/flect`). Link it somewhere on your `PATH`:

```sh
ln -s ~/Applications/Flect-for-Mac.app/Contents/Resources/flect /usr/local/bin/flect
```

From a source checkout: `cd core && cargo build --release`, then use
`core/target/release/flect` (keep `flect-engine` in the same folder).

## Commands

```
flect list [--json]
flect run <task> [-o key=value ...] [--json-options '{...}'] [--json] <paths...>
flect run --preset <name> [--json] <paths...>
flect run folder.removeDuplicate|folder.removeEmpty [-o ...] [--apply [--permanent]] <paths...>
flect settings path
```

- `flect list` prints one runnable task per line: `id<TAB>label<TAB>options`. Presets and
  "Custom..." entries of the menu are folded into their task (`image.resize`), and conversions
  show their targets (`to=UTF8|UTF8BOM|...`). With `--json` it prints a JSON array.
- `flect run` takes the task id, any options, and one or more paths (relative paths are made
  absolute; the order is kept, which matters for `pdf.merge`). Options are the same parameters the
  app passes, e.g. `flect run image.resize -o scale=50 photo.png`,
  `flect run text.lineEndings -o to=CRLF notes.txt`. `--json-options '{"scale":50}'` does the
  same from a flat JSON object (booleans become `1`/`0`). See the protocol document for each
  task's parameters. Use `--` before paths that start with `-`. An option overrides the same
  name in the task's own query, and a later option overrides an earlier one, so each name reaches
  the engine once: `flect run 'image.resize?scale=50' -o scale=30 photo.png` resizes to 30 %.
  Names match exactly as written (case-sensitive, not decoded): `Scale` or `%73cale` in the query
  is another name, and the engine refuses a malformed or repeated name (`task.badOption`, exit 1).
- `flect run --preset NAME` runs a preset saved in the app (Settings > Presets): the exact name,
  or the preset's id. A preset with several steps runs as one pipeline: intermediate files stay
  in a private folder under `$TMPDIR` that is removed afterwards, and only the final file is
  written next to each original. `presets.json` is only read (and checked strictly); a missing,
  invalid or newer-version file is an error (exit 1) and is left as it is.
- `flect settings path` prints the settings file (`FLECT_HOME` overrides the data folder).

Results are written next to the originals under new names; existing files are never overwritten.

### Deleting tasks (`folder.removeDuplicate`, `folder.removeEmpty`)

A terminal has no confirmation dialog, so these tasks **never delete by default**. Without
`--apply` the CLI forces the preview form (duplicates: `dry=1`; empty folders: no `confirm=1`),
prints what would be removed, and deletes nothing; `dry` / `confirm` given with `-o` are ignored. Option names in the task are compared after percent-decoding and without regard to case, so every spelling of `dry` / `confirm` is dropped; a name that is not plain `[A-Za-z][A-Za-z0-9_]*` is a usage error (exit 2), and the engine itself refuses encoded or repeated option names.

- `--apply` makes the task really delete. With the default setting (Trash) the engine only asks
  the app to move the files to the Trash, which the CLI cannot do, so nothing is removed.
- If your setting is "delete permanently", `--apply` alone is refused: add `--permanent` as well
  (`flect run folder.removeEmpty --apply --permanent dir`). Permanent deletion cannot be undone.
- `--permanent` without `--apply`, and either flag on other tasks, is a usage error (exit 2).

## Output

Human mode (default): status, info, warnings, errors, `[done/total]` progress and a final summary
go to **stderr**; data results (for example the tree from `folder.tree`) go to **stdout**, so
`flect run folder.tree ~/Documents > tree.txt` works. The core carries no message texts
(localization belongs to the app), so messages appear as the message key plus its arguments,
e.g. `warn: msg.alreadySmall: photo.png`.

`--json`: stdout is the engine's event stream, one JSON object per line, unchanged
(`status`, `step`, `info`, `warn`, `error`, `data`, `needs`, `done`). Nothing else is written to
stdout. Parse the last line (`done`) for the outcome.

Tasks that need the app's OS services (`needs: trash`, used when removal is confirmed with the
Trash setting) are reported but not performed; the CLI never moves anything to the Trash. The
engine sends one `needs` event per item (with its fingerprint, and for a duplicate the copy that
stays); the CLI does not ask the engine to wait for answers (`trashAck`), so it gets them all at
once. In human mode it prints one note with the number of items and ends with "nothing changed"
instead of the engine's "moved to the Trash" summary; with `--json` each `needs` line is passed on
as it is. The deleting tasks also send a `data` event named `removal` (`items`, `leftAlone`) for
the app's history; human mode does not print it. The only deletion the CLI can perform is
permanent deletion, which needs `--apply --permanent`.

## Exit status

| Code | Meaning |
| --- | --- |
| 0 | Succeeded |
| 1 | Failed (some file failed, unknown task, engine missing or ended early) |
| 2 | Usage error |
| 130 | Cancelled |

Ctrl-C (or SIGTERM) sends the protocol `cancel`: the running task stops after the current file
and `flect` exits 130; originals are never modified. A second Ctrl-C kills the engine immediately.
