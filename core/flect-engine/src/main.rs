//! flect-engine: reads JSON Lines requests on stdin, writes events on stdout.
//!
//! A reader thread parses stdin; `cancel` takes effect immediately (it only flips a flag the
//! running task polls), everything else is queued for the worker, which runs one request at a time.
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use flect_core::folder::Progress;
use flect_core::media::{Capabilities, Tools};
use flect_core::mediatasks::{self, MediaEnv};
use flect_core::protocol::{parse_line, Event, Outcome, Request, MAX_LINE_BYTES};
use flect_core::settings::HardwareEncoder;
use flect_core::settings::{Settings, Store};
use flect_core::{archive, batch, jar, locations, menu, tasks};

type Out = Arc<Mutex<io::Stdout>>;

fn emit(out: &Out, event: &Event) -> io::Result<()> {
    let mut line = serde_json::to_vec(event)?;
    line.push(b'\n');
    // One lock per event so lines from the reader and worker threads never interleave.
    let mut guard = out.lock().unwrap_or_else(|e| e.into_inner());
    guard.write_all(&line)?;
    guard.flush()
}

fn error(key: &str, args: Vec<String>) -> Event {
    Event::Error {
        key: key.into(),
        args,
    }
}

fn failed(out: &Out, key: &str, args: Vec<String>) -> io::Result<()> {
    emit(out, &error(key, args))?;
    emit(
        out,
        &Event::Done {
            outcome: Outcome::Failed,
            summary_key: None,
            summary_args: vec![],
        },
    )
}

/// `image.convert?to=PNG` -> `image.convert`; known when it names a catalog entry or its parent.
fn known_task(task: &str) -> bool {
    let name = task.split('?').next().unwrap_or("");
    !name.is_empty()
        && menu::leaf_ids(&menu::catalog()).iter().any(|id| {
            *id == name
                || id
                    .strip_prefix(name)
                    .is_some_and(|rest| rest.starts_with('.'))
        })
}

/// Read one line, never buffering more than the protocol limit.
/// `Some(true)` means the line was too long and has been discarded.
fn read_bounded(input: &mut impl BufRead, buf: &mut Vec<u8>) -> io::Result<Option<bool>> {
    buf.clear();
    let mut too_long = false;
    loop {
        let chunk = input.fill_buf()?;
        if chunk.is_empty() {
            return Ok(if buf.is_empty() && !too_long {
                None
            } else {
                Some(too_long)
            });
        }
        let (take, done) = match chunk.iter().position(|b| *b == b'\n') {
            Some(i) => (i + 1, true),
            None => (chunk.len(), false),
        };
        if !too_long {
            buf.extend_from_slice(&chunk[..take]);
            if buf.len() > MAX_LINE_BYTES + 1 {
                too_long = true;
                buf.clear();
            }
        }
        input.consume(take);
        if done {
            return Ok(Some(too_long));
        }
    }
}

enum Msg {
    Run { task: String, paths: Vec<String> },
}

/// stdin -> queue. Handles everything that must not wait behind a running task.
fn reader(out: Out, cancel: Arc<AtomicBool>, tx: mpsc::Sender<Msg>) -> io::Result<()> {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut buf = Vec::new();
    while let Some(too_long) = read_bounded(&mut input, &mut buf)? {
        if too_long {
            failed(&out, "protocol.lineTooLong", vec![])?;
            continue;
        }
        if buf.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        match parse_line(&buf) {
            Err(e) => failed(&out, e.key(), vec![])?,
            Ok(Request::ExportMenu) => emit(
                &out,
                &Event::Menu {
                    menu: menu::export(),
                },
            )?,
            Ok(Request::Cancel) => cancel.store(true, Ordering::SeqCst),
            Ok(Request::Run { task, paths, .. }) => {
                if tx.send(Msg::Run { task, paths }).is_err() {
                    break;
                }
            }
        }
    }
    Ok(())
}

/// Turns task progress into protocol events and exposes the cancel flag.
struct EngineProgress<'a> {
    out: &'a Out,
    cancel: &'a AtomicBool,
}

impl Progress for EngineProgress<'_> {
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }
    fn status(&self, key: &str, args: &[String]) {
        let _ = emit(
            self.out,
            &Event::Status {
                key: key.into(),
                args: args.to_vec(),
            },
        );
    }
    fn step(&self, done: u64, total: u64) {
        let _ = emit(self.out, &Event::Step { done, total });
    }
    fn file_step(&self, permille: u32) {
        let _ = emit(self.out, &Event::FileStep { permille });
    }
}

fn display_name(p: &str) -> String {
    Path::new(p)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn run_files(out: &Out, cancel: &AtomicBool, task: &str, paths: &[String]) -> io::Result<()> {
    let settings: Settings = locations::data_dir()
        .map(|d| Store::new(d).load())
        .unwrap_or_default();
    let seven_zip = archive::find_tool("7zz");
    let is_cancelled = || cancel.load(Ordering::SeqCst);
    let progress = EngineProgress { out, cancel };
    // ffmpeg is looked for (and probed) only when a media task actually runs.
    let name = task.split('?').next().unwrap_or("");
    let media_env = mediatasks::is_media_task(name)
        .then(|| {
            Tools::discover().map(|tools| MediaEnv {
                caps: Capabilities::detect(
                    &tools,
                    settings.video.hardware == HardwareEncoder::VideoToolbox,
                ),
                tools,
            })
        })
        .flatten();
    // A real JDK and the pinned CFR jar are only looked for when decompiling.
    let java = (name == "jar.decompile").then(jar::find_java).flatten();
    let cfr = locations::data_dir().map(|d| d.join("deps/cfr.jar"));
    let ctx = tasks::Ctx {
        settings: &settings,
        seven_zip: seven_zip.as_deref(),
        progress: &progress,
        media: media_env.as_ref(),
        java: java.as_deref(),
        cfr: cfr.as_deref(),
    };
    let total = paths.len() as u64;
    let mut failures = 0;
    for (i, p) in paths.iter().enumerate() {
        if is_cancelled() {
            return emit(
                out,
                &Event::Done {
                    outcome: Outcome::Cancelled,
                    summary_key: None,
                    summary_args: vec![],
                },
            );
        }
        let shown = display_name(p);
        emit(
            out,
            &Event::Status {
                key: "msg.processing".into(),
                args: vec![shown.clone()],
            },
        )?;
        match tasks::run(task, Path::new(p), &ctx) {
            Ok(done) => {
                for (key, args) in done.infos {
                    emit(
                        out,
                        &Event::Info {
                            key: key.into(),
                            args,
                        },
                    )?;
                }
                for (key, args) in done.warnings {
                    emit(
                        out,
                        &Event::Warn {
                            key: key.into(),
                            args,
                        },
                    )?;
                }
            }
            Err(_) if is_cancelled() => {
                return emit(
                    out,
                    &Event::Done {
                        outcome: Outcome::Cancelled,
                        summary_key: None,
                        summary_args: vec![],
                    },
                )
            }
            Err(e) => {
                failures += 1;
                emit(out, &error(e.key(), vec![shown]))?;
            }
        }
        emit(
            out,
            &Event::Step {
                done: i as u64 + 1,
                total,
            },
        )?;
    }
    let outcome = if failures == 0 {
        Outcome::Succeeded
    } else {
        Outcome::Failed
    };
    emit(
        out,
        &Event::Done {
            outcome,
            summary_key: None,
            summary_args: vec![],
        },
    )
}

fn run_batch(out: &Out, cancel: &AtomicBool, task: &str, paths: &[String]) -> io::Result<()> {
    let settings: Settings = locations::data_dir()
        .map(|d| Store::new(d).load())
        .unwrap_or_default();
    let is_cancelled = || cancel.load(Ordering::SeqCst);
    let failure = Mutex::new(None::<io::Error>);
    let emit = |e: Event| {
        if let Err(err) = emit(out, &e) {
            *failure.lock().unwrap_or_else(|p| p.into_inner()) = Some(err);
        }
    };
    batch::run_batch(
        task,
        paths,
        &batch::Ctx {
            settings: &settings,
            cancelled: &is_cancelled,
            emit: &emit,
        },
    );
    failure
        .into_inner()
        .unwrap_or_else(|p| p.into_inner())
        .map_or(Ok(()), Err)
}

fn main() -> io::Result<()> {
    let out: Out = Arc::new(Mutex::new(io::stdout()));
    let cancel = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    let handle = {
        let (out, cancel) = (out.clone(), cancel.clone());
        std::thread::spawn(move || reader(out, cancel, tx))
    };
    for msg in rx {
        let Msg::Run { task, paths } = msg;
        cancel.store(false, Ordering::SeqCst); // a stale cancel must not kill the next run
        let name = task.split('?').next().unwrap_or("");
        if batch::is_batch_task(name) {
            run_batch(&out, &cancel, &task, &paths)?;
        } else if tasks::is_file_task(name) {
            run_files(&out, &cancel, &task, &paths)?;
        } else if known_task(&task) {
            failed(&out, "task.unimplemented", vec![task])?;
        } else {
            failed(&out, "task.unknown", vec![task])?;
        }
    }
    handle
        .join()
        .map_err(|_| io::Error::other("reader thread panicked"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_tasks() {
        assert!(known_task("image.resize.p1"));
        assert!(known_task("image.convert?to=PNG"));
        assert!(known_task("video.compress"));
        assert!(!known_task("image.nope"));
        assert!(!known_task("imag"));
        assert!(!known_task(""));
    }

    #[test]
    fn bounded_reader_survives_oversized_lines() {
        let mut data = vec![b'a'; MAX_LINE_BYTES * 3];
        data.push(b'\n');
        data.extend_from_slice(b"{\"op\":\"cancel\"}\n");
        let mut r = io::Cursor::new(data);
        let mut buf = Vec::new();
        assert_eq!(read_bounded(&mut r, &mut buf).unwrap(), Some(true));
        assert_eq!(read_bounded(&mut r, &mut buf).unwrap(), Some(false));
        assert!(parse_line(&buf).is_ok());
        assert_eq!(read_bounded(&mut r, &mut buf).unwrap(), None);
    }
}
