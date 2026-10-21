//! `flect`: command-line front end for Flect-for-Mac.
//!
//! Design: this binary does no file processing itself. It spawns the sibling `flect-engine`
//! (the same separate process the app uses) and speaks the JSON-Lines protocol with it
//! (`docs/mac-port/08-engine-protocol.ja.md`). That keeps the README's security design intact: all
//! parsing of untrusted files stays in the engine process, no shell is involved (the engine is
//! started with an argument-less `Command`), and both crates are `forbid(unsafe_code)`. Calling
//! `flect-core` directly would be less code, but would put file parsing into a process that also
//! handles terminal I/O and signals, and the CLI could drift from what the app does. The engine
//! is looked up only next to this executable (symlinks resolved), never through `PATH` or an
//! environment variable, and refused if it is writable by group/others, like the engine does
//! for the external tools it launches.
#![forbid(unsafe_code)]

mod args;
mod guard;
mod render;

use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};

use serde_json::{json, Value};
use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook::iterator::Signals;

use args::{Command as Cmd, RunArgs};

const EXIT_OK: i32 = 0;
const EXIT_FAILED: i32 = 1;
const EXIT_USAGE: i32 = 2;
const EXIT_CANCELLED: i32 = 130;

enum Input {
    Line(Vec<u8>),
    Eof,
    Signal,
}

struct Engine {
    child: Child,
    stdin: Option<ChildStdin>,
    rx: Receiver<Input>,
}

fn engine_path() -> Result<PathBuf, String> {
    let exe = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map_err(|e| format!("cannot locate flect: {e}"))?;
    let path = exe
        .parent()
        .ok_or("cannot locate flect-engine")?
        .join("flect-engine");
    let meta = path.metadata().map_err(|e| {
        format!(
            "flect-engine not found next to flect ({}): {e}",
            path.display()
        )
    })?;
    if !meta.is_file() || meta.permissions().mode() & 0o022 != 0 {
        return Err(format!(
            "refusing to run {}: not a regular file or writable by others",
            path.display()
        ));
    }
    Ok(path)
}

impl Engine {
    fn spawn() -> Result<Self, String> {
        let signals = Signals::new([SIGINT, SIGTERM]).map_err(|e| format!("signals: {e}"))?;
        let mut child = Command::new(engine_path()?)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            // Own process group: a terminal Ctrl-C reaches us, and we turn it into a clean Cancel.
            .process_group(0)
            .spawn()
            .map_err(|e| format!("cannot start flect-engine: {e}"))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().ok_or("no engine stdout")?;
        let (tx, rx) = mpsc::channel();
        spawn_reader(stdout, tx.clone());
        spawn_signals(signals, tx);
        Ok(Self { child, stdin, rx })
    }

    fn send(&mut self, request: &Value) -> bool {
        let Some(stdin) = self.stdin.as_mut() else {
            return false;
        };
        let mut line = request.to_string();
        line.push('\n');
        stdin
            .write_all(line.as_bytes())
            .and_then(|()| stdin.flush())
            .is_ok()
    }

    fn finish(mut self) {
        drop(self.stdin.take()); // EOF on stdin lets the engine exit
        let _ = self.child.wait();
    }
}

fn spawn_reader(stdout: std::process::ChildStdout, tx: Sender<Input>) {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut buf = Vec::new();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if tx.send(Input::Line(buf)).is_err() {
                        return;
                    }
                }
            }
        }
        let _ = tx.send(Input::Eof);
    });
}

fn spawn_signals(mut signals: Signals, tx: Sender<Input>) {
    std::thread::spawn(move || {
        for _ in signals.forever() {
            if tx.send(Input::Signal).is_err() {
                return;
            }
        }
    });
}

fn say(text: &str) {
    let _ = writeln!(io::stderr(), "{text}");
}

/// Write to stdout; `false` once the reader went away (e.g. `| head`).
fn emit_out(text: &str) -> bool {
    let mut out = io::stdout().lock();
    writeln!(out, "{text}").and_then(|()| out.flush()).is_ok()
}

fn list(json_output: bool) -> i32 {
    let mut engine = match Engine::spawn() {
        Ok(e) => e,
        Err(e) => return fail(&e),
    };
    engine.send(&json!({"op": "export-menu"}));
    let mut menu = None;
    while let Ok(Input::Line(line)) = engine.rx.recv() {
        if let Ok(v) = serde_json::from_slice::<Value>(&line) {
            if v["event"] == "menu" {
                menu = Some(v["menu"].clone());
                break;
            }
        }
    }
    engine.finish();
    let Some(menu) = menu else {
        return fail("flect-engine did not return its menu");
    };
    let tasks = render::task_list(&menu);
    if json_output {
        let items: Vec<Value> = tasks
            .iter()
            .map(|t| json!({"id": t.id, "label": t.label, "to": t.to}))
            .collect();
        emit_out(&Value::Array(items).to_string());
    } else {
        for t in &tasks {
            if !emit_out(&render::task_line(t)) {
                break;
            }
        }
    }
    EXIT_OK
}

fn fail(message: &str) -> i32 {
    say(&format!("flect: {}", render::safe(message)));
    EXIT_FAILED
}

/// The request for `--preset NAME`: `run` for one step, `pipeline` for more. The app writes
/// `presets.json`; here it is only read, through the same strict checks the engine applies.
fn preset_request(a: &RunArgs, name: &str, setting_permanent: bool) -> Result<Value, String> {
    let paths = &a.paths;
    use flect_core::presets::{self, PresetError};
    let dir = flect_core::locations::data_dir()
        .ok_or("cannot determine the data folder (HOME is not set)")?;
    let all = match presets::load(&dir) {
        Ok(p) => p,
        Err(PresetError::Missing) => {
            return Err(
                "no presets have been saved yet (create them in the app's Presets tab)".into(),
            )
        }
        Err(e) => return Err(e.to_string()),
    };
    let preset = presets::find(&all, name).ok_or_else(|| format!("no preset named {name:?}"))?;
    Ok(match preset.steps.as_slice() {
        [one] => {
            let task = guard::guard_task(one, a.apply, a.permanent, setting_permanent)?;
            json!({"op": "run", "task": task, "paths": paths})
        }
        steps => json!({"op": "pipeline", "steps": steps, "paths": paths}),
    })
}

fn run(a: RunArgs) -> i32 {
    let setting_permanent = flect_core::locations::data_dir()
        .map(|d| flect_core::settings::Store::new(d).load())
        .is_some_and(|s| s.folder.delete_method == flect_core::settings::DeleteMethod::Permanent);
    let request = match &a.preset {
        Some(name) => match preset_request(&a, name, setting_permanent) {
            Ok(r) => r,
            Err(e) if e.starts_with("--") || e.starts_with("your setting") => {
                fail(&e);
                return EXIT_USAGE;
            }
            Err(e) => return fail(&e),
        },
        None => match guard::guard_task(&a.task, a.apply, a.permanent, setting_permanent) {
            Ok(task) => json!({"op": "run", "task": task, "paths": a.paths}),
            Err(e) => {
                fail(&e);
                return EXIT_USAGE;
            }
        },
    };
    if !a.apply
        && (a.task.starts_with("folder.removeDuplicate")
            || a.task.starts_with("folder.removeEmpty"))
    {
        say("flect: preview only, nothing is deleted (add --apply to delete)");
    }
    let mut engine = match Engine::spawn() {
        Ok(e) => e,
        Err(e) => return fail(&e),
    };
    if !engine.send(&request) {
        return fail("flect-engine closed its input");
    }
    let (mut signals, mut stdout_ok) = (0u32, true);
    let mut tally = render::Tally::default();
    let mut code = None;
    while code.is_none() {
        match engine.rx.recv() {
            Ok(Input::Signal) => {
                signals += 1;
                if signals == 1 {
                    say("cancelling... (Ctrl-C again to stop immediately)");
                    engine.send(&json!({"op": "cancel"}));
                } else {
                    let _ = engine.child.kill();
                    code = Some(EXIT_CANCELLED);
                }
            }
            Ok(Input::Line(line)) => {
                if a.json && stdout_ok {
                    stdout_ok = emit_out(String::from_utf8_lossy(&line).trim_end());
                    if !stdout_ok {
                        engine.send(&json!({"op": "cancel"}));
                    }
                }
                if let Ok(v) = serde_json::from_slice::<Value>(&line) {
                    code = handle_event(&v, a.json, &mut tally);
                }
            }
            Ok(Input::Eof) | Err(_) => {
                say("flect: flect-engine ended without a result");
                code = Some(if signals > 0 {
                    EXIT_CANCELLED
                } else {
                    EXIT_FAILED
                });
            }
        }
    }
    engine.finish();
    code.unwrap_or(EXIT_FAILED)
}

/// Print one engine event; returns the exit code when it is the final `done`.
fn handle_event(v: &Value, json_output: bool, tally: &mut render::Tally) -> Option<i32> {
    let event = v["event"].as_str().unwrap_or("");
    if event == "error" {
        tally.errors += 1;
    }
    if event == "step" {
        tally.done = v["done"].as_u64().unwrap_or(0);
        tally.total = v["total"].as_u64().unwrap_or(0);
    }
    if event == "done" {
        let code = match v["outcome"].as_str() {
            Some("succeeded") => EXIT_OK,
            Some("cancelled") => EXIT_CANCELLED,
            _ => EXIT_FAILED,
        };
        if !json_output {
            let outcome = v["outcome"].as_str().unwrap_or("failed");
            if tally.trash > 0 {
                say(&format!(
                    "note: the app would move {} item(s) to the Trash; the CLI does not, so nothing was changed",
                    tally.trash
                ));
            }
            let summary = if tally.trash > 0 {
                Some(format!(
                    "nothing changed; {} item(s) would go to the Trash in the app",
                    tally.trash
                ))
            } else {
                v["summary_key"].as_str().map(|key| {
                    let args: Vec<&str> = v["summary_args"]
                        .as_array()
                        .map(|a| a.iter().filter_map(Value::as_str).collect())
                        .unwrap_or_default();
                    render::plain(key, &args)
                })
            };
            say(&format!(
                "{}: {}/{} file(s), {} error(s){}",
                render::safe(outcome),
                tally.done,
                tally.total,
                tally.errors,
                summary.map(|s| format!(" ({s})")).unwrap_or_default()
            ));
        }
        return Some(code);
    }
    if json_output {
        return None;
    }
    match event {
        "status" => say(&format!("-> {}", render::message(v))),
        "step" => say(&format!("[{}/{}]", tally.done, tally.total)),
        "info" => say(&format!("info: {}", render::message(v))),
        "warn" => say(&format!("warn: {}", render::message(v))),
        "error" => say(&format!("error: {}", render::message(v))),
        "needs" => {
            // Counted, and reported once at the end (one item per event).
            tally.trash += v["paths"].as_array().map_or(0, |a| a.len() as u64);
        }
        // the deleting tasks' counts for the app's history; the summary line already says it
        "data" if v["name"] == "removal" => {}
        "data" => {
            // Results go to stdout so `flect run folder.tree dir > tree.txt` works.
            let text = match (v["name"].as_str(), v["data"]["text"].as_str()) {
                (Some("tree"), Some(t)) => render::safe_multiline(t),
                _ => render::safe_multiline(
                    &serde_json::to_string_pretty(&v["data"]).unwrap_or_default(),
                ),
            };
            emit_out(&text);
        }
        _ => {}
    }
    None
}

fn settings_path() -> i32 {
    // Same file name as flect_core::settings::Store (checked by a test against Store::save).
    match flect_core::locations::data_dir() {
        Some(dir) => {
            emit_out(&dir.join("settings.json").to_string_lossy());
            EXIT_OK
        }
        None => fail("cannot determine the data folder (HOME is not set)"),
    }
}

fn main() {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => std::process::exit(fail(&format!("cannot read the current directory: {e}"))),
    };
    let code = match args::parse(args, &cwd) {
        Ok(Cmd::Help) => {
            emit_out(args::USAGE);
            EXIT_OK
        }
        Ok(Cmd::Version) => {
            emit_out(&format!("flect {}", env!("CARGO_PKG_VERSION")));
            EXIT_OK
        }
        Ok(Cmd::List { json }) => list(json),
        Ok(Cmd::SettingsPath) => settings_path(),
        Ok(Cmd::Run(a)) => run(a),
        Err(message) => {
            say(&format!(
                "flect: {}\n\n{}",
                render::safe(&message),
                args::USAGE
            ));
            EXIT_USAGE
        }
    };
    std::process::exit(code);
}
