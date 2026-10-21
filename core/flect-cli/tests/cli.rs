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

const PRESETS: &str = r#"{"version":1,"presets":[
  {"id":"6F1C2A8E-0B7D-4C1E-9A55-3D2B1C0E9F11","name":"Unix UTF-16","steps":["text.lineEndings?to=LF","text.encode?to=UTF16"]},
  {"id":"0E4C7A1B-9D3E-4F2A-8B6C-5A4D3C2B1A00","name":"CRLF","steps":["text.lineEndings?to=CRLF"]}
]}"#;

#[test]
fn preset_runs_its_steps_and_writes_only_the_final_file() {
    let (home, work, tmp) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    fs::write(home.path().join("presets.json"), PRESETS).unwrap();
    let src = work.path().join("n.txt");
    fs::write(&src, "a\r\nb\r\n").unwrap();
    let out = flect(home.path())
        .env("TMPDIR", tmp.path())
        .args(["run", "--preset", "Unix UTF-16", src.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(names(work.path()), ["n.txt", "n_lf_utf16.txt"]);
    assert_eq!(fs::read(&src).unwrap(), b"a\r\nb\r\n");
    assert!(names(tmp.path()).is_empty(), "work folder left behind");
    // one step: an ordinary run; the id works as well as the name
    let out = run(
        home.path(),
        &[
            "run",
            "--preset",
            "0e4c7a1b-9d3e-4f2a-8b6c-5a4d3c2b1a00",
            src.to_str().unwrap(),
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(work.path().join("n_crlf.txt").exists());
    // the presets file is only read, never rewritten
    assert_eq!(
        fs::read_to_string(home.path().join("presets.json")).unwrap(),
        PRESETS
    );
}

#[test]
fn unknown_missing_or_invalid_presets_fail_without_touching_anything() {
    let (home, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let src = work.path().join("n.txt");
    fs::write(&src, "a\n").unwrap();
    let arg = src.to_str().unwrap();
    let out = run(home.path(), &["run", "--preset", "CRLF", arg]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("no presets"),
        "{}",
        text(&out.stderr)
    );
    fs::write(home.path().join("presets.json"), PRESETS).unwrap();
    let out = run(home.path(), &["run", "--preset", "crlf", arg]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("no preset named"));
    let hostile = r#"{"version":1,"presets":[{"id":"6F1C2A8E-0B7D-4C1E-9A55-3D2B1C0E9F11","name":"x","steps":["folder.removeDuplicate?mode=all"]}]}"#;
    fs::write(home.path().join("presets.json"), hostile).unwrap();
    let out = run(home.path(), &["run", "--preset", "x", arg]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("invalid"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(
        fs::read_to_string(home.path().join("presets.json")).unwrap(),
        hostile
    );
    assert_eq!(names(work.path()), ["n.txt"]);
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
fn deleting_tasks_preview_unless_apply_and_permanent() {
    use flect_core::settings::{DeleteMethod, Settings, Store};
    let (home, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut s = Settings::default();
    s.folder.delete_method = DeleteMethod::Permanent;
    Store::new(home.path().to_path_buf()).save(&s).unwrap();
    let empty = work.path().join("hollow");
    fs::create_dir(&empty).unwrap();
    let root = work.path().to_str().unwrap();
    // default and a user-supplied confirm=1: preview only
    for extra in [&[][..], &["-o", "confirm=1"][..]] {
        let mut args = vec!["run", "folder.removeEmpty"];
        args.extend_from_slice(extra);
        args.push(root);
        run(home.path(), &args);
        assert!(empty.is_dir(), "deleted without --apply");
    }
    // --apply alone is refused while the setting is permanent
    let out = run(home.path(), &["run", "folder.removeEmpty", "--apply", root]);
    assert_eq!(out.status.code(), Some(2));
    assert!(empty.is_dir());
    let out = run(
        home.path(),
        &["run", "folder.removeEmpty", "--permanent", root],
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(empty.is_dir());
    // both flags: really deleted
    let out = run(
        home.path(),
        &["run", "folder.removeEmpty", "--apply", "--permanent", root],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(!empty.exists());
}

/// The guard stripped `dry` / `confirm` by their raw spelling while the engine percent-decoded
/// option names, so `folder.removeEmpty?%63onfirm=1` deleted permanently without `--apply`.
#[test]
fn encoded_or_disguised_option_names_cannot_bypass_apply() {
    use flect_core::settings::{DeleteMethod, Settings, Store};
    let (home, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut s = Settings::default();
    s.folder.delete_method = DeleteMethod::Permanent;
    s.folder.duplicates.keep = flect_core::settings::DuplicateKeep::Oldest;
    Store::new(home.path().to_path_buf()).save(&s).unwrap();
    let empty = work.path().join("hollow");
    fs::create_dir(&empty).unwrap();
    fs::write(work.path().join("a.txt"), "same").unwrap();
    fs::write(work.path().join("b.txt"), "same").unwrap();
    let root = work.path().to_str().unwrap();
    let intact = || {
        empty.is_dir() && work.path().join("a.txt").exists() && work.path().join("b.txt").exists()
    };
    for task in [
        "folder.removeEmpty?%63onfirm=1",
        "folder.removeEmpty?%63%6F%6E%66%69%72%6D=1",
        "folder.removeEmpty?%43onfirm=1",
        "folder.removeEmpty?%63onfirm=%31",
        "folder.removeEmpty?Confirm=1",
        "folder.removeEmpty?CONFIRM=1",
        "folder.removeEmpty?confirm=1&confirm=1",
        "folder.removeEmpty?confirm=0&confirm=1",
        "folder.removeEmpty?+confirm=1",
        "folder.removeEmpty?%20confirm=1",
        "folder.removeEmpty?confirm%20=1",
        "folder.removeEmpty? confirm=1",
        "folder.removeDuplicate?mode=all&%64ry=0",
        "folder.removeDuplicate?mode=all&%44RY=0",
        "folder.removeDuplicate?mode=all&dry=1&dry=0",
        "folder.removeDuplicate?mode=all&dry=0&%64ry=0",
        "folder.removeDuplicate?mode=all&DRY=0",
        "folder.removeDuplicate?mode=all&+dry=0",
    ] {
        let out = run(home.path(), &["run", task, root]);
        assert!(
            intact(),
            "{task}: deleted without --apply (exit {:?}): {}{}",
            out.status.code(),
            text(&out.stdout),
            text(&out.stderr)
        );
        assert_ne!(out.status.code(), Some(130), "{task}");
    }
}

/// `-o` repeating a name of the task's query reached the engine twice and was refused
/// (`task.badOption`, exit 1). The option now overrides it; the deleting-task guard still applies.
#[test]
fn an_option_overrides_the_same_name_in_the_task() {
    let (home, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let src = work.path().join("a.txt");
    fs::write(&src, "one\ntwo\n").unwrap();
    let out = run(
        home.path(),
        &[
            "run",
            "text.lineEndings?to=LF",
            "-o",
            "to=CRLF",
            src.to_str().unwrap(),
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(!text(&out.stderr).contains("task.badOption"));
    let made: Vec<String> = names(work.path())
        .into_iter()
        .filter(|n| n != "a.txt")
        .collect();
    assert_eq!(made.len(), 1, "{made:?}");
    assert_eq!(
        fs::read(work.path().join(&made[0])).unwrap(),
        b"one\r\ntwo\r\n",
        "the -o value wins"
    );

    // -o dry=0 / confirm=1 overriding the query still cannot delete without --apply
    let empty = work.path().join("hollow");
    fs::create_dir(&empty).unwrap();
    fs::write(work.path().join("b.txt"), "one\ntwo\n").unwrap();
    let root = work.path().to_str().unwrap();
    for args in [
        &["folder.removeEmpty?confirm=0", "-o", "confirm=1"][..],
        &["folder.removeDuplicate?mode=all&dry=1", "-o", "dry=0"][..],
        &[
            "folder.removeDuplicate?mode=all",
            "-o",
            "dry=0",
            "-o",
            "mode=all",
        ][..],
    ] {
        let mut v = vec!["run"];
        v.extend_from_slice(args);
        v.push(root);
        let out = run(home.path(), &v);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{args:?}: {}",
            text(&out.stderr)
        );
        assert!(
            empty.is_dir() && src.exists() && work.path().join("b.txt").exists(),
            "{args:?} deleted without --apply"
        );
    }
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

/// With the Trash setting, the CLI says once how many items the app would move, and does not
/// claim anything was moved.
#[test]
fn trash_requests_are_summarised_once() {
    let (home, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    for i in 0..3 {
        fs::write(work.path().join(format!("a{i}")), format!("{i}")).unwrap();
        fs::write(work.path().join(format!("b{i}")), format!("{i}")).unwrap();
    }
    let out = run(
        home.path(),
        &[
            "run",
            "folder.removeDuplicate?mode=oldest",
            "--apply",
            work.path().to_str().unwrap(),
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let err = text(&out.stderr);
    assert_eq!(err.matches("would move").count(), 1, "{err}");
    assert!(err.contains("would move 3 item(s) to the Trash"), "{err}");
    assert!(err.contains("nothing changed"), "{err}");
    assert!(!err.contains("msg.recycled"), "{err}");
    assert_eq!(names(work.path()).len(), 6, "nothing was moved");
}
