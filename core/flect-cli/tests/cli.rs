//! End-to-end tests: run the real `flect` binary, which spawns the real `flect-engine`.
//! Everything happens in temp dirs; `FLECT_HOME` points at one so user settings are never read.
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Once;

use serde_json::Value;

fn flect_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_flect"))
}

/// `flect` looks for `flect-engine` next to itself; make sure it has been built for this profile.
fn ensure_engine() {
    static BUILD: Once = Once::new();
    BUILD.call_once(|| {
        let dir = flect_bin().parent().unwrap().to_path_buf();
        let mut cmd = Command::new(env!("CARGO"));
        cmd.args([
            "build",
            "--locked",
            "-q",
            "-p",
            "flect-engine",
            "--manifest-path",
        ])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../Cargo.toml"));
        if dir.ends_with("release") {
            cmd.arg("--release");
        }
        assert!(
            cmd.status().unwrap().success(),
            "building flect-engine failed"
        );
        assert!(dir.join("flect-engine").is_file());
    });
}

fn flect(home: &Path) -> Command {
    ensure_engine();
    let mut c = Command::new(flect_bin());
    c.env("FLECT_HOME", home).stdin(Stdio::null());
    c
}

fn run(home: &Path, args: &[&str]) -> Output {
    flect(home).args(args).output().unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn converts_line_endings_and_leaves_original_alone() {
    let (home, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let src = work.path().join("a.txt");
    fs::write(&src, "one\ntwo\n").unwrap();
    let out = run(
        home.path(),
        &[
            "run",
            "text.lineEndings",
            "-o",
            "to=CRLF",
            src.to_str().unwrap(),
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(fs::read_to_string(&src).unwrap(), "one\ntwo\n");
    let others: Vec<String> = names(work.path())
        .into_iter()
        .filter(|n| n != "a.txt")
        .collect();
    assert_eq!(others.len(), 1, "{others:?}");
    assert_eq!(
        fs::read(work.path().join(&others[0])).unwrap(),
        b"one\r\ntwo\r\n"
    );
    let err = text(&out.stderr);
    assert!(err.contains("msg.processing: a.txt"), "{err}");
    assert!(err.contains("succeeded: 1/1 file(s), 0 error(s)"), "{err}");
}

#[test]
fn relative_paths_and_json_stream() {
    let (home, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    fs::write(work.path().join("b.json"), r#"{"b":1,"a":2}"#).unwrap();
    let out = flect(home.path())
        .current_dir(work.path())
        .args(["run", "json.pretty", "--json", "b.json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let events: Vec<Value> = text(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).expect("every stdout line is JSON"))
        .collect();
    assert_eq!(events[0]["event"], "status");
    let last = events.last().unwrap();
    assert_eq!(last["event"], "done");
    assert_eq!(last["outcome"], "succeeded");
    assert_eq!(
        fs::read_to_string(work.path().join("b.json")).unwrap(),
        r#"{"b":1,"a":2}"#
    );
}

#[test]
fn tree_prints_to_stdout() {
    let (home, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    fs::create_dir(work.path().join("sub")).unwrap();
    fs::write(work.path().join("sub/inner.txt"), "x").unwrap();
    let out = run(
        home.path(),
        &["run", "folder.tree", work.path().to_str().unwrap()],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    assert!(stdout.contains("inner.txt"), "{stdout}");
    assert!(
        !stdout.contains("msg."),
        "progress belongs on stderr: {stdout}"
    );
    assert_eq!(names(work.path()), ["sub"]);
}

#[test]
fn failure_exits_one_and_unknown_task_too() {
    let home = tempfile::tempdir().unwrap();
    let missing = home.path().join("missing.txt");
    let out = run(
        home.path(),
        &[
            "run",
            "text.lineEndings",
            "-o",
            "to=LF",
            missing.to_str().unwrap(),
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("error:"),
        "{}",
        text(&out.stderr)
    );
    assert!(!missing.exists());
    let out = run(home.path(), &["run", "no.such.task", "/tmp"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("task.unknown"));
}

#[test]
fn usage_errors_exit_two() {
    let home = tempfile::tempdir().unwrap();
    for args in [
        &[][..],
        &["bogus"],
        &["run"],
        &["run", "text.encode"],
        &["run", "t", "--nope", "x"],
    ] {
        let out = run(home.path(), args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(text(&out.stderr).contains("Usage:"));
    }
    assert_eq!(run(home.path(), &["--help"]).status.code(), Some(0));
}

#[test]
fn list_shows_runnable_tasks() {
    let home = tempfile::tempdir().unwrap();
    let out = run(home.path(), &["list"]);
    assert_eq!(out.status.code(), Some(0));
    let stdout = text(&out.stdout);
    assert!(
        stdout.lines().any(|l| l.starts_with("folder.tree\t")),
        "{stdout}"
    );
    let convert = stdout
        .lines()
        .find(|l| l.starts_with("text.encode\t"))
        .unwrap();
    assert!(convert.contains("to=UTF8|UTF8BOM"), "{convert}");
    let json: Value =
        serde_json::from_slice(&run(home.path(), &["list", "--json"]).stdout).unwrap();
    assert!(json
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["id"] == "image.resize"));
}

#[test]
fn settings_path_matches_the_store() {
    let home = tempfile::tempdir().unwrap();
    flect_core::settings::Store::new(home.path().to_path_buf())
        .save(&flect_core::settings::Settings::default())
        .unwrap();
    let out = run(home.path(), &["settings", "path"]);
    assert_eq!(out.status.code(), Some(0));
    let printed = PathBuf::from(text(&out.stdout).trim());
    assert_eq!(printed, home.path().join("settings.json"));
    assert!(
        printed.is_file(),
        "Store writes a different file name than the CLI reports"
    );
}

#[test]
fn ctrl_c_cancels_with_130_and_keeps_originals() {
    let (home, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    // Each result is fsynced, so thousands of files take long enough to interrupt reliably.
    let files: Vec<String> = (0..4000)
        .map(|i| {
            let p = work.path().join(format!("f{i:04}.txt"));
            fs::write(&p, "a\nb\n").unwrap();
            p.to_string_lossy().into_owned()
        })
        .collect();
    let mut child = flect(home.path())
        .args(["run", "text.lineEndings", "-o", "to=CRLF", "--json", "--"])
        .args(&files)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    assert!(lines.next().unwrap().unwrap().contains("status"));
    assert!(Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap()
        .success());
    let rest: Vec<String> = lines.map(Result::unwrap).collect();
    assert_eq!(child.wait().unwrap().code(), Some(130));
    assert!(
        rest.last().unwrap().contains("\"outcome\":\"cancelled\""),
        "{:?}",
        rest.last()
    );
    for f in &files {
        assert_eq!(fs::read_to_string(f).unwrap(), "a\nb\n");
    }
    assert!(
        names(work.path()).len() < 2 * files.len(),
        "cancel should stop before the end"
    );
}
