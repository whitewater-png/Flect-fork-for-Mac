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
  task's parameters. Use `--` before paths that start with `-`.
- `flect run --preset NAME` runs a preset saved in the app (Settings > Presets): the exact name,
  or the preset's id. A preset with several steps runs as one pipeline: intermediate files stay
  in a private folder under `$TMPDIR` that is removed afterwards, and only the final file is
  written next to each original. `presets.json` is only read (and checked strictly); a missing,
  invalid or newer-version file is an error (exit 1) and is left as it is.
- `flect settings path` prints the settings file (`FLECT_HOME` overrides the data folder).

Results are written next to the originals under new names; existing files are never overwritten.

## Output

Human mode (default): status, info, warnings, errors, `[done/total]` progress and a final summary
go to **stderr**; data results (for example the tree from `folder.tree`) go to **stdout**, so
`flect run folder.tree ~/Documents > tree.txt` works. The core carries no message texts
(localization belongs to the app), so messages appear as the message key plus its arguments,
e.g. `warn: msg.alreadySmall: photo.png`.

`--json`: stdout is the engine's event stream, one JSON object per line, unchanged
(`status`, `step`, `info`, `warn`, `error`, `data`, `needs`, `done`). Nothing else is written to
stdout. Parse the last line (`done`) for the outcome.

Tasks that need the app's OS services (`needs: trash`, used after confirming duplicate or empty
folder removal) are reported but not performed; the CLI never moves anything to the Trash.

## Exit status

| Code | Meaning |
| --- | --- |
| 0 | Succeeded |
| 1 | Failed (some file failed, unknown task, engine missing or ended early) |
| 2 | Usage error |
| 130 | Cancelled |

Ctrl-C (or SIGTERM) sends the protocol `cancel`: the running task stops after the current file
and `flect` exits 130; originals are never modified. A second Ctrl-C kills the engine immediately.
