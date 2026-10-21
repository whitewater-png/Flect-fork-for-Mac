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
    // the files this run created are reported exactly (the app's Undo relies on it)
    let out = ev
        .iter()
        .find(|x| x["event"] == "data" && x["name"] == "outputs")
        .expect("outputs event");
    assert_eq!(
        out["data"]["paths"][0],
        d.path().join("a_sorted.json").to_str().unwrap()
    );
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

/// A tiny JPEG-shaped file with EXIF GPS (35.6586 N, 139.7453 E) in front of a fake scan.
fn jpeg_with_gps() -> Vec<u8> {
    let mut t = b"II*\0\x08\0\0\0".to_vec();
    let w16 = |t: &mut Vec<u8>, v: u16| t.extend_from_slice(&v.to_le_bytes());
    let w32 = |t: &mut Vec<u8>, v: u32| t.extend_from_slice(&v.to_le_bytes());
    // IFD0 at 8: one entry, the GPS IFD pointer (26)
    w16(&mut t, 1);
    w16(&mut t, 0x8825);
    w16(&mut t, 4);
    w32(&mut t, 1);
    w32(&mut t, 26);
    w32(&mut t, 0);
    // GPS IFD at 26: 4 entries, values at 80 and 104
    w16(&mut t, 4);
    for (tag, ty, count, value) in [
        (1u16, 2u16, 2u32, u32::from(b'N')),
        (2, 5, 3, 80),
        (3, 2, 2, u32::from(b'E')),
        (4, 5, 3, 104),
    ] {
        w16(&mut t, tag);
        w16(&mut t, ty);
        w32(&mut t, count);
        w32(&mut t, value);
    }
    w32(&mut t, 0);
    for (d, m, s) in [(35u32, 39u32, 31u32), (139, 44, 43)] {
        for v in [d, m, s] {
            w32(&mut t, v);
            w32(&mut t, 1);
        }
    }
    let mut body = b"Exif\0\0".to_vec();
    body.extend_from_slice(&t);
    let mut j = vec![0xFF, 0xD8, 0xFF, 0xE1];
    j.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
    j.extend_from_slice(&body);
    j.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x02, 0xAB, 0xCD, 0xFF, 0xD9]);
    j
}

#[test]
fn privacy_check_reports_findings_and_writes_nothing_then_make_safe_copies() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("trip.jpg");
    let original = jpeg_with_gps();
    std::fs::write(&f, &original).unwrap();
    let mut e = Engine::start(&d.path().join("home"));

    e.send(&format!(
        r#"{{"op":"run","task":"image.privacy","paths":["{}"]}}"#,
        f.display()
    ));
    let ev = e.until_done();
    assert_eq!(ev.last().unwrap()["outcome"], "succeeded");
    let report = ev
        .iter()
        .find(|x| x["event"] == "data" && x["name"] == "privacy_report")
        .expect("privacy_report event");
    assert_eq!(report["data"]["items"][0]["kind"], "gps");
    assert_eq!(report["data"]["items"][0]["detail"], "35.6586, 139.7453");
    assert!(ev
        .iter()
        .any(|x| x["event"] == "info" && x["key"] == "msg.privacyFound"));
    assert!(
        !ev.iter()
            .any(|x| x["event"] == "data" && x["name"] == "outputs"),
        "a read-only check creates nothing"
    );
    assert!(!d.path().join("trip_safe.jpg").exists());

    e.send(&format!(
        r#"{{"op":"run","task":"image.makeSafe","paths":["{}"]}}"#,
        f.display()
    ));
    let ev = e.until_done();
    assert_eq!(ev.last().unwrap()["outcome"], "succeeded");
    let safe = d.path().join("trip_safe.jpg");
    assert!(safe.exists());
    let out = ev
        .iter()
        .find(|x| x["event"] == "data" && x["name"] == "outputs")
        .expect("outputs event (powers Undo)");
    assert_eq!(out["data"]["paths"][0], safe.to_str().unwrap());
    assert_eq!(std::fs::read(&f).unwrap(), original, "source untouched");
    let cleaned = std::fs::read(&safe).unwrap();
    assert!(!cleaned.windows(4).any(|w| w == b"Exif"));
}

#[test]
fn duplicate_preview_deletes_nothing_and_confirming_asks_for_the_trash() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("a.bin"), "same").unwrap();
    std::fs::write(d.path().join("b.bin"), "same").unwrap();
    let mut e = Engine::start(&d.path().join("home"));
    e.send(&format!(
        r#"{{"op":"run","task":"folder.removeDuplicate?mode=oldest&dry=1","paths":["{}"]}}"#,
        d.path().display()
    ));
    let ev = e.until_done();
    assert!(ev
        .iter()
        .any(|x| x["event"] == "data" && x["name"] == "duplicates_preview"));
    assert!(!ev.iter().any(|x| x["event"] == "needs"));
    assert!(d.path().join("a.bin").exists() && d.path().join("b.bin").exists());
    e.send(&format!(
        r#"{{"op":"run","task":"folder.removeDuplicate?mode=oldest","paths":["{}"]}}"#,
        d.path().display()
    ));
    let ev = e.until_done();
    let needs = ev.iter().find(|x| x["event"] == "needs").unwrap();
    assert_eq!(needs["what"], "trash");
}
