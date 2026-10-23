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
        Self::start_in(home, &std::env::temp_dir())
    }
    /// With `TMPDIR` pointing at `tmp`, so pipeline work folders can be checked.
    fn start_in(home: &std::path::Path, tmp: &std::path::Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_flect-engine"))
            .env("FLECT_HOME", home)
            .env("TMPDIR", tmp)
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

fn names(dir: &std::path::Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn outputs(ev: &[serde_json::Value]) -> Vec<String> {
    ev.iter()
        .filter(|x| x["event"] == "data" && x["name"] == "outputs")
        .flat_map(|x| x["data"]["paths"].as_array().unwrap().clone())
        .map(|p| p.as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn pipeline_reports_only_the_final_output_and_leaves_no_work_files() {
    let (d, tmp) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let f = d.path().join("a.txt");
    std::fs::write(&f, "one\r\ntwo\r\n").unwrap();
    let mut e = Engine::start_in(&d.path().join("home"), tmp.path());
    e.send(&format!(
        r#"{{"op":"pipeline","steps":["text.lineEndings?to=LF","text.encode?to=UTF16"],"paths":["{}"]}}"#,
        f.display()
    ));
    let ev = e.until_done();
    assert_eq!(ev.last().unwrap()["outcome"], "succeeded", "{ev:?}");
    let last = d.path().join("a_lf_utf16.txt");
    assert_eq!(outputs(&ev), [last.to_str().unwrap()]);
    assert_eq!(names(d.path()), ["a.txt", "a_lf_utf16.txt"]);
    assert_eq!(std::fs::read(&f).unwrap(), b"one\r\ntwo\r\n");
    let stages = ev
        .iter()
        .filter(|x| x["event"] == "status" && x["key"] == "msg.pipelineStage")
        .count();
    assert_eq!(stages, 2);
    // the private work folder is gone by the time `done` arrives
    assert!(names(tmp.path()).is_empty(), "{:?}", names(tmp.path()));
}

#[test]
fn invalid_pipelines_are_refused_before_anything_is_written() {
    let (d, tmp) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let f = d.path().join("a.txt");
    std::fs::write(&f, "x").unwrap();
    let mut e = Engine::start_in(&d.path().join("home"), tmp.path());
    for (steps, key) in [
        (
            r#"["text.encode?to=UTF8","image.strip"]"#,
            "msg.pipelineIncompatible",
        ),
        (
            r#"["folder.removeEmpty?confirm=1"]"#,
            "msg.pipelineStepNotAllowed",
        ),
        (r#"["text.encode?to=EBCDIC"]"#, "msg.pipelineBadStep"),
    ] {
        e.send(&format!(
            r#"{{"op":"pipeline","steps":{steps},"paths":["{}"]}}"#,
            f.display()
        ));
        let ev = e.until_done();
        assert_eq!(ev[0]["key"], key);
        assert_eq!(ev.last().unwrap()["outcome"], "failed");
    }
    e.send(r#"{"op":"pipeline","steps":[],"paths":["/x"]}"#);
    assert_eq!(e.until_done()[0]["key"], "protocol.badPipeline");
    // a file of the wrong kind fails alone; the others still run
    let j = d.path().join("b.json");
    std::fs::write(&j, "{\"b\":1,\"a\":2}").unwrap();
    let img = d.path().join("c.png");
    std::fs::write(&img, "not really").unwrap();
    e.send(&format!(
        r#"{{"op":"pipeline","steps":["json.sort","json.pretty"],"paths":["{}","{}"]}}"#,
        img.display(),
        j.display()
    ));
    let ev = e.until_done();
    assert_eq!(ev.last().unwrap()["outcome"], "failed");
    assert!(ev.iter().any(|x| x["key"] == "msg.pipelineWrongInput"));
    assert_eq!(
        outputs(&ev),
        [d.path().join("b_sorted_pretty.json").to_str().unwrap()]
    );
    assert_eq!(
        names(d.path()),
        ["a.txt", "b.json", "b_sorted_pretty.json", "c.png"]
    );
    assert!(names(tmp.path()).is_empty());
}

#[test]
fn cancel_stops_a_pipeline_and_removes_its_work_folder() {
    let (d, tmp) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
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
    let mut e = Engine::start_in(&d.path().join("home"), tmp.path());
    e.send(&format!(
        r#"{{"op":"pipeline","steps":["json.pretty","json.sort"],"paths":[{}]}}"#,
        paths.join(",")
    ));
    let mut seen = Vec::new();
    loop {
        let x = e.next();
        let stop = x["event"] == "step";
        seen.push(x);
        if stop {
            break;
        }
    }
    let t = Instant::now();
    e.send(r#"{"op":"cancel"}"#);
    seen.extend(e.until_done());
    assert_eq!(seen.last().unwrap()["outcome"], "cancelled");
    assert!(
        t.elapsed() < Duration::from_secs(10),
        "cancel took {:?}",
        t.elapsed()
    );
    // exactly the reported outputs exist next to the originals; no intermediate leaked out
    let made: Vec<String> = names(d.path())
        .into_iter()
        .filter(|n| n.contains("_pretty"))
        .collect();
    assert_eq!(made.len(), outputs(&seen).len());
    assert!(
        made.iter().all(|n| n.ends_with("_pretty_sorted.json")),
        "{made:?}"
    );
    assert!(made.len() < 100);
    assert!(names(tmp.path()).is_empty(), "{:?}", names(tmp.path()));
}

/// With `options.trashAck`, the engine hands on one Trash item at a time and waits for
/// `{"op":"trash-ack"}`; a cancel while it waits stops the run. Without the option nothing waits.
#[test]
fn trash_requests_wait_for_the_apps_answer_when_asked() {
    let d = tempfile::tempdir().unwrap();
    for i in 0..3 {
        std::fs::write(d.path().join(format!("a{i}")), format!("{i}")).unwrap();
        std::fs::write(d.path().join(format!("b{i}")), format!("{i}")).unwrap();
    }
    let mut e = Engine::start(&d.path().join("home"));
    let run = format!(
        r#"{{"op":"run","task":"folder.removeDuplicate?mode=oldest","paths":["{}"],"options":{{"trashAck":true}}}}"#,
        d.path().display()
    );
    e.send(&run);
    let mut needs = 0;
    loop {
        let ev = e.next();
        if ev["event"] == "needs" {
            needs += 1;
            assert_eq!(ev["ack"], true, "{ev}");
            assert!(ev["items"][0]["kept"]["fingerprint"].is_string(), "{ev}");
            if needs == 2 {
                e.send(r#"{"op":"cancel"}"#);
            } else {
                e.send(r#"{"op":"trash-ack"}"#);
            }
        }
        if ev["event"] == "done" {
            assert_eq!(ev["outcome"], "cancelled", "{ev}");
            assert_eq!(ev["summary_key"], "msg.cancelledAfterTrashing");
            break;
        }
    }
    assert_eq!(needs, 2, "the third item was never handed on");
    // without the option: all three at once, no ack asked
    e.send(&format!(
        r#"{{"op":"run","task":"folder.removeDuplicate?mode=oldest","paths":["{}"]}}"#,
        d.path().display()
    ));
    let ev = e.until_done();
    let needs: Vec<_> = ev.iter().filter(|x| x["event"] == "needs").collect();
    assert_eq!(needs.len(), 3);
    assert!(needs.iter().all(|n| n.get("ack").is_none()));
    // a stray answer is harmless
    e.send(r#"{"op":"trash-ack"}"#);
    e.send(r#"{"op":"export-menu"}"#);
    assert_eq!(e.next()["event"], "menu");
}

#[test]
fn download_is_refused_until_the_user_turns_it_on() {
    let d = tempfile::tempdir().unwrap();
    let mut e = Engine::start(&d.path().join("home"));
    e.send(r#"{"op":"download","url":"https://example.com/v","kind":"audio"}"#);
    let ev = e.until_done();
    assert!(
        ev.iter()
            .any(|x| x["event"] == "error" && x["key"] == "download.disabled"),
        "{ev:?}"
    );
    assert_eq!(ev.last().unwrap()["outcome"], "failed");
    assert!(
        !ev.iter().any(|x| x["event"] == "data"),
        "nothing may be reported as created"
    );
}

#[test]
fn bad_download_requests_are_refused_and_the_engine_keeps_serving() {
    let d = tempfile::tempdir().unwrap();
    let mut e = Engine::start(&d.path().join("home"));
    for (line, key) in [
        (
            r#"{"op":"download","url":"--exec=x","kind":"audio"}"#,
            "protocol.badUrl",
        ),
        (
            r#"{"op":"download","url":"https://a.example/","kind":"playlist"}"#,
            "protocol.badDownload",
        ),
        (
            r#"{"op":"download","url":"https://a.example/","kind":"audio","extra":1}"#,
            "protocol.badJson",
        ),
    ] {
        e.send(line);
        let ev = e.until_done();
        assert!(
            ev.iter().any(|x| x["event"] == "error" && x["key"] == key),
            "{line}: {ev:?}"
        );
    }
    e.send(r#"{"op":"export-menu"}"#);
    assert_eq!(e.next()["event"], "menu");
}
