//! Drives the real binary over stdio.
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

struct Engine {
    child: Child,
    stdin: ChildStdin,
    lines: BufReader<ChildStdout>,
}

impl Engine {
    fn start(home: &std::path::Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_flect-engine"))
            .env("FLECT_HOME", home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let lines = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            lines,
        }
    }
    fn send(&mut self, json: &str) {
        writeln!(self.stdin, "{json}").unwrap();
    }
    fn next(&mut self) -> serde_json::Value {
        let mut l = String::new();
        assert!(
            self.lines.read_line(&mut l).unwrap() > 0,
            "engine closed stdout"
        );
        serde_json::from_str(&l).unwrap()
    }
    /// Events up to and including the next `done`.
    fn until_done(&mut self) -> Vec<serde_json::Value> {
        let mut v = Vec::new();
        loop {
            let e = self.next();
            let done = e["event"] == "done";
            v.push(e);
            if done {
                return v;
            }
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

#[test]
fn runs_a_text_task_and_reports_events() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("a.json");
    std::fs::write(&f, "{\"b\":1,\"a\":2}").unwrap();
    let mut e = Engine::start(&d.path().join("home"));
    e.send(&format!(
        r#"{{"op":"run","task":"json.sort","paths":["{}"]}}"#,
        f.display()
    ));
    let ev = e.until_done();
    assert_eq!(ev.last().unwrap()["outcome"], "succeeded");
    assert!(ev.iter().any(|x| x["event"] == "step" && x["done"] == 1));
    assert!(d.path().join("a_sorted.json").exists());
}

#[test]
fn rejects_garbage_but_keeps_serving() {
    let d = tempfile::tempdir().unwrap();
    let mut e = Engine::start(d.path());
    e.send("not json");
    assert_eq!(e.until_done()[0]["key"], "protocol.badJson");
    e.send(r#"{"op":"export-menu"}"#);
    assert_eq!(e.next()["event"], "menu");
    e.send(r#"{"op":"run","task":"nope.nope","paths":["/tmp/x"]}"#);
    assert_eq!(e.until_done()[0]["key"], "task.unknown");
}

#[test]
fn cancel_stops_a_long_batch_quickly() {
    let d = tempfile::tempdir().unwrap();
    // 150 files of ~1.5 MiB of JSON: far more work than the test is willing to wait for.
    let body = format!(
        "[{}]",
        (0..100_000)
            .map(|i| format!("{{\"k{i}\":{i}}}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    let paths: Vec<String> = (0..150)
        .map(|i| {
            let p = d.path().join(format!("f{i}.json"));
            std::fs::write(&p, &body).unwrap();
            format!("\"{}\"", p.display())
        })
        .collect();
    let mut e = Engine::start(&d.path().join("home"));
    e.send(&format!(
        r#"{{"op":"run","task":"json.pretty","paths":[{}]}}"#,
        paths.join(",")
    ));
    loop {
        if e.next()["event"] == "step" {
            break;
        }
    }
    let t = Instant::now();
    e.send(r#"{"op":"cancel"}"#);
    let ev = e.until_done();
    assert_eq!(ev.last().unwrap()["outcome"], "cancelled");
    assert!(
        t.elapsed() < Duration::from_secs(5),
        "cancel took {:?}",
        t.elapsed()
    );
    let steps = ev.iter().filter(|x| x["event"] == "step").count();
    assert!(
        steps < 100,
        "{steps} files were still processed after cancel"
    );
    // the next run is not affected by the earlier cancel
    let one = d.path().join("f0.json");
    e.send(&format!(
        r#"{{"op":"run","task":"json.sort","paths":["{}"]}}"#,
        one.display()
    ));
    assert_eq!(e.until_done().last().unwrap()["outcome"], "succeeded");
}
