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
use flect_core::{
    archive, batch, download, jar, locations, menu, paths, pipeline, pool, privacy, tasks,
};

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
    Run {
        task: String,
        paths: Vec<String>,
        jobs: Option<usize>,
        /// The app answers every Trash request (`trash-ack`).
        trash_ack: bool,
    },
    Pipeline {
        steps: Vec<String>,
        paths: Vec<String>,
    },
    Download {
        url: String,
        kind: download::Kind,
    },
}

/// stdin -> queue. Handles everything that must not wait behind a running task.
fn reader(
    out: Out,
    cancel: Arc<AtomicBool>,
    tx: mpsc::Sender<Msg>,
    acks: mpsc::Sender<()>,
) -> io::Result<()> {
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
            Ok(Request::TrashAck) => {
                let _ = acks.send(());
            }
            Ok(Request::Run {
                task,
                paths,
                options,
            }) => {
                let jobs = options
                    .get("jobs")
                    .and_then(serde_json::Value::as_u64)
                    .map(|n| n as usize);
                let trash_ack = options
                    .get("trashAck")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                if tx
                    .send(Msg::Run {
                        task,
                        paths,
                        jobs,
                        trash_ack,
                    })
                    .is_err()
                {
                    break;
                }
            }
            Ok(Request::Pipeline { steps, paths }) => {
                if tx.send(Msg::Pipeline { steps, paths }).is_err() {
                    break;
                }
            }
            Ok(Request::Download {
                url,
                kind,
                max_height,
            }) => {
                // `parse_line` has already checked both; this only turns them into the typed form.
                let Some(kind) = download::kind_from(&kind, max_height) else {
                    failed(&out, "protocol.badDownload", vec![])?;
                    continue;
                };
                if tx.send(Msg::Download { url, kind }).is_err() {
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

fn done(out: &Out, outcome: Outcome) -> io::Result<()> {
    emit(
        out,
        &Event::Done {
            outcome,
            summary_key: None,
            summary_args: vec![],
        },
    )
}

/// A per-file failure as a message key and its arguments.
trait Failure {
    fn key(&self) -> &'static str;
    fn args(&self, shown: String) -> Vec<String>;
}

impl Failure for tasks::TaskError {
    fn key(&self) -> &'static str {
        tasks::TaskError::key(self)
    }
    fn args(&self, shown: String) -> Vec<String> {
        tasks::TaskError::args(self, shown)
    }
}

impl Failure for pipeline::RunError {
    fn key(&self) -> &'static str {
        pipeline::RunError::key(self)
    }
    fn args(&self, shown: String) -> Vec<String> {
        pipeline::RunError::args(self, shown)
    }
}

/// Builds what file tasks need; tools are looked for only when one of `names` uses them.
fn with_ctx<R>(
    out: &Out,
    cancel: &AtomicBool,
    names: &[&str],
    f: impl FnOnce(&tasks::Ctx<'_>) -> R,
) -> R {
    let settings: Settings = locations::data_dir()
        .map(|d| Store::new(d).load())
        .unwrap_or_default();
    let seven_zip = archive::find_tool("7zz");
    let progress = EngineProgress { out, cancel };
    // ffmpeg is looked for (and probed) only when a media task actually runs.
    let media_env = names
        .iter()
        .any(|n| mediatasks::is_media_task(n))
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
    let java = names
        .contains(&"jar.decompile")
        .then(jar::find_java)
        .flatten();
    let cfr = locations::data_dir().map(|d| d.join("deps/cfr.jar"));
    let ctx = tasks::Ctx {
        settings: &settings,
        seven_zip: seven_zip.as_deref(),
        progress: &progress,
        media: media_env.as_ref(),
        java: java.as_deref(),
        cfr: cfr.as_deref(),
    };
    f(&ctx)
}

/// What parallel workers get: cancellation only (no per-file progress events).
struct QuietProgress<'a> {
    cancel: &'a AtomicBool,
}

impl Progress for QuietProgress<'_> {
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }
}

fn run_files(
    out: &Out,
    cancel: &AtomicBool,
    task: &str,
    paths: &[String],
    jobs: Option<usize>,
) -> io::Result<()> {
    let name = task.split('?').next().unwrap_or("");
    let workers = if pool::is_parallel_task(name) {
        pool::worker_count(paths.len(), jobs)
    } else {
        1
    };
    let outcome = if workers > 1 {
        run_files_parallel(out, cancel, task, paths, workers)?
    } else {
        with_ctx(out, cancel, &[name], |ctx| {
            each_file(out, cancel, paths, name == privacy::REPORT_TASK, |_, p| {
                tasks::run(task, p, ctx)
            })
        })?
    };
    done(out, outcome)
}

/// Independent image/text files: run them side by side, report them in input order.
/// Workers see cancellation only; per-file progress would interleave meaninglessly.
fn run_files_parallel(
    out: &Out,
    cancel: &AtomicBool,
    task: &str,
    paths: &[String],
    workers: usize,
) -> io::Result<Outcome> {
    let settings: Settings = locations::data_dir()
        .map(|d| Store::new(d).load())
        .unwrap_or_default();
    let name = task.split('?').next().unwrap_or("");
    let report = name == privacy::REPORT_TASK;
    let is_cancelled = || cancel.load(Ordering::SeqCst);
    // Set when stdout broke: stop handing out new files instead of processing the whole batch
    // with nobody left to report to.
    let broken = AtomicBool::new(false);
    let stop = || is_cancelled() || broken.load(Ordering::SeqCst);
    let quiet = QuietProgress { cancel };
    let work = |i: usize| {
        let wctx = tasks::Ctx {
            settings: &settings,
            seven_zip: None,
            progress: &quiet,
            media: None,
            java: None,
            cfr: None,
        };
        tasks::run(task, Path::new(&paths[i]), &wctx)
    };
    let total = paths.len() as u64;
    let mut failures = 0;
    let mut io_error = None;
    let mut interrupted = false;
    let delivered = pool::run_ordered(paths.len(), workers, &stop, &work, |u| {
        if io_error.is_some() {
            return;
        }
        let r = match u {
            pool::Update::Waiting(i) if !is_cancelled() => status(out, &paths[i]).map(|()| true),
            pool::Update::Waiting(_) => Ok(true),
            pool::Update::Finished(i, res) => deliver(
                out,
                &is_cancelled,
                (i, total),
                &paths[i],
                report,
                &mut failures,
                res,
            ),
        };
        match r {
            Ok(ok) => interrupted |= !ok,
            Err(e) => {
                broken.store(true, Ordering::SeqCst);
                io_error = Some(e);
            }
        }
    });
    if let Some(e) = io_error {
        return Err(e);
    }
    if interrupted || delivered < paths.len() {
        return Ok(Outcome::Cancelled);
    }
    Ok(outcome_of(failures))
}

/// Several steps per file (`op: pipeline`). Intermediate files live in a private folder under
/// `$TMPDIR`, which is gone before `done` is sent; only final results are reported as outputs.
fn run_pipeline(
    out: &Out,
    cancel: &AtomicBool,
    steps: &[String],
    paths: &[String],
) -> io::Result<()> {
    let plan = match pipeline::plan(steps) {
        Ok(p) => p,
        Err(e) => return failed(out, e.key(), e.args()),
    };
    let work = match pipeline::WorkDir::create(&std::env::temp_dir()) {
        Ok(w) => w,
        Err(_) => {
            return failed(
                out,
                "msg.ioError",
                vec![std::env::temp_dir().display().to_string()],
            )
        }
    };
    let names: Vec<&str> = plan.iter().map(|s| s.name).collect();
    let outcome = with_ctx(out, cancel, &names, |ctx| {
        each_file(out, cancel, paths, false, |i, p| {
            pipeline::run_file(&plan, p, ctx, &work, i)
        })
    });
    // Removed before `done`: the app may stop the engine as soon as it has read it.
    drop(work);
    done(out, outcome?)
}

/// One web download (`op: download`). Everything the tool needs is checked first, so a missing
/// tool or a switched-off feature is explained instead of failing somewhere inside.
fn run_download(out: &Out, cancel: &AtomicBool, url: &str, kind: download::Kind) -> io::Result<()> {
    let settings: Settings = locations::data_dir()
        .map(|d| Store::new(d).load())
        .unwrap_or_default();
    let tool = match download::preflight(&settings, download::find(), Tools::discover().is_some()) {
        Ok(t) => t,
        Err(key) => return failed(out, key, vec![]),
    };
    let Ok(url) = download::validate_url(url) else {
        return failed(out, "protocol.badUrl", vec![]);
    };
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .filter(|h| h.is_absolute());
    let Some(home) = home else {
        return failed(out, "msg.ioError", vec![]);
    };
    let dest = match download::destination(&settings, &home) {
        Ok(d) => d,
        Err(e) => return failed(out, paths::io_error_key(&e), vec![]),
    };
    let progress = EngineProgress { out, cancel };
    progress.status("download.starting", &[download::host_of(&url)]);
    let tmp = std::env::temp_dir();
    let job = download::Job {
        tool: &tool,
        url: &url,
        kind,
        out_dir: &dest,
        env: download::Env {
            path: download::PATH_ENV,
            home: Some(&home),
        },
        work_parent: &tmp,
    };
    match download::download(&job, &progress) {
        Ok(f) if f.created.is_empty() && f.existing.is_empty() => {
            failed(out, "download.noFile", vec![])
        }
        Ok(f) => {
            if !f.created.is_empty() {
                let paths: Vec<_> = f.created.iter().map(|p| p.to_string_lossy()).collect();
                emit(
                    out,
                    &Event::Data {
                        name: "outputs".into(),
                        data: serde_json::json!({ "paths": paths }),
                    },
                )?;
            }
            for p in &f.created {
                emit(
                    out,
                    &Event::Info {
                        key: "download.saved".into(),
                        args: vec![display_name(&p.to_string_lossy())],
                    },
                )?;
            }
            for p in &f.existing {
                emit(
                    out,
                    &Event::Warn {
                        key: "download.alreadyExists".into(),
                        args: vec![display_name(&p.to_string_lossy())],
                    },
                )?;
            }
            // A file that was already there counts as done, but it is not reported as new.
            done(out, Outcome::Succeeded)
        }
        Err(download::RunError::Cancelled) => done(out, Outcome::Cancelled),
        Err(download::RunError::Stalled) => failed(out, "download.stalled", vec![]),
        Err(download::RunError::Failed { tail, .. }) => failed(out, "download.failed", vec![tail]),
        Err(download::RunError::Io(_)) => failed(out, "download.cantStart", vec![]),
    }
}

fn outcome_of(failures: u32) -> Outcome {
    if failures == 0 {
        Outcome::Succeeded
    } else {
        Outcome::Failed
    }
}

fn status(out: &Out, path: &str) -> io::Result<()> {
    emit(
        out,
        &Event::Status {
            key: "msg.processing".into(),
            args: vec![display_name(path)],
        },
    )
}

/// Reports one finished file: the only place outputs, messages and `step` come from, serial or not.
/// `false` means the run was cancelled.
fn deliver<E: Failure>(
    out: &Out,
    is_cancelled: &dyn Fn() -> bool,
    (i, total): (usize, u64),
    p: &str,
    privacy_report: bool,
    failures: &mut u32,
    result: Result<tasks::Done, E>,
) -> io::Result<bool> {
    // The privacy check shows its findings as data; the task itself writes nothing.
    if privacy_report {
        if let Ok(event) = privacy::report_event(Path::new(p)) {
            emit(out, &event)?;
        }
    }
    match result {
        Ok(done) => {
            // Tell the app exactly what this run created (it powers "Undo"); a task that left the input as is reports nothing.
            if done.output != Path::new(p) {
                emit(
                    out,
                    &Event::Data {
                        name: "outputs".into(),
                        data: serde_json::json!({ "paths": [done.output.to_string_lossy()] }),
                    },
                )?;
            }
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
        Err(_) if is_cancelled() => return Ok(false),
        Err(e) => {
            *failures += 1;
            emit(out, &error(e.key(), e.args(display_name(p))))?;
        }
    }
    emit(
        out,
        &Event::Step {
            done: i as u64 + 1,
            total,
        },
    )?;
    Ok(true)
}

/// Runs `job` on each path with the per-file events (status, outputs, messages, step) and returns
/// how the run ended; the caller sends `done`.
fn each_file<E: Failure>(
    out: &Out,
    cancel: &AtomicBool,
    paths: &[String],
    privacy_report: bool,
    mut job: impl FnMut(usize, &Path) -> Result<tasks::Done, E>,
) -> io::Result<Outcome> {
    let is_cancelled = || cancel.load(Ordering::SeqCst);
    let total = paths.len() as u64;
    let mut failures = 0;
    for (i, p) in paths.iter().enumerate() {
        if is_cancelled() {
            return Ok(Outcome::Cancelled);
        }
        status(out, p)?;
        let result = job(i, Path::new(p));
        if !deliver(
            out,
            &is_cancelled,
            (i, total),
            p,
            privacy_report,
            &mut failures,
            result,
        )? {
            return Ok(Outcome::Cancelled);
        }
    }
    Ok(outcome_of(failures))
}

/// How long the engine waits for the app to answer one Trash request before it stops the run.
const TRASH_ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

fn run_batch(
    out: &Out,
    cancel: &AtomicBool,
    task: &str,
    paths: &[String],
    acks: Option<&Mutex<mpsc::Receiver<()>>>,
) -> io::Result<()> {
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
    // One answer per Trash request: wait for it, but never past a cancel or the timeout.
    let wait = || {
        let Some(rx) = acks else { return true };
        let rx = rx.lock().unwrap_or_else(|p| p.into_inner());
        let started = std::time::Instant::now();
        loop {
            match rx.recv_timeout(std::time::Duration::from_millis(50)) {
                Ok(()) => return true,
                Err(mpsc::RecvTimeoutError::Disconnected) => return false,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if is_cancelled() || started.elapsed() > TRASH_ACK_TIMEOUT {
                        return false;
                    }
                }
            }
        }
    };
    batch::run_batch(
        task,
        paths,
        &batch::Ctx {
            settings: &settings,
            cancelled: &is_cancelled,
            emit: &emit,
            trash_ack: acks.map(|_| &wait as &(dyn Fn() -> bool + Sync)),
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
    let (ack_tx, ack_rx) = mpsc::channel();
    let acks = Mutex::new(ack_rx);
    let handle = {
        let (out, cancel) = (out.clone(), cancel.clone());
        std::thread::spawn(move || reader(out, cancel, tx, ack_tx))
    };
    for msg in rx {
        cancel.store(false, Ordering::SeqCst); // a stale cancel must not kill the next run
                                               // stale answers must not count for the next run
        while acks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .try_recv()
            .is_ok()
        {}
        let (task, paths, jobs, trash_ack) = match msg {
            Msg::Run {
                task,
                paths,
                jobs,
                trash_ack,
            } => (task, paths, jobs, trash_ack),
            Msg::Pipeline { steps, paths } => {
                run_pipeline(&out, &cancel, &steps, &paths)?;
                continue;
            }
            Msg::Download { url, kind } => {
                run_download(&out, &cancel, &url, kind)?;
                continue;
            }
        };
        let name = task.split('?').next().unwrap_or("");
        if tasks::check_task(&task).is_err() {
            // Option names are taken literally and must be well-formed and unique; anything else
            // is refused before a task runs (a guard that reads the names sees what runs).
            failed(&out, "task.badOption", vec![task])?;
        } else if batch::is_batch_task(name) {
            run_batch(&out, &cancel, &task, &paths, trash_ack.then_some(&acks))?;
        } else if tasks::is_file_task(name) {
            run_files(&out, &cancel, &task, &paths, jobs)?;
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
