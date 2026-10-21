//! Parallel batches through the real binary: same events and files as a serial run.
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};

fn run(home: &Path, jobs: &str, request: &str) -> Vec<serde_json::Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_flect-engine"))
        .env("FLECT_HOME", home)
        .env("FLECT_JOBS", jobs)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    writeln!(stdin, "{request}").unwrap();
    let mut out = Vec::new();
    for line in BufReader::new(child.stdout.take().unwrap()).lines() {
        let e: serde_json::Value = serde_json::from_str(&line.unwrap()).unwrap();
        let done = e["event"] == "done";
        out.push(e);
        if done {
            break;
        }
    }
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    out
}

fn request(paths: &[String]) -> String {
    serde_json::json!({"op": "run", "task": "json.pretty", "paths": paths}).to_string()
}

fn shape(events: &[serde_json::Value], root: &Path) -> Vec<String> {
    events
        .iter()
        .map(|e| e.to_string().replace(root.to_str().unwrap(), "<root>"))
        .collect()
}

#[test]
fn parallel_run_reports_exactly_what_a_serial_run_reports() {
    let mut shapes = Vec::new();
    for jobs in ["1", "6"] {
        let d = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let mut paths = Vec::new();
        for i in 0..30 {
            let p = d.path().join(format!("f{i:02}.json"));
            if i == 7 {
                std::fs::write(&p, "{not json").unwrap(); // fails; the others carry on
            } else {
                std::fs::write(&p, format!("{{\"b\":{i},\"a\":[1,2]}}")).unwrap();
            }
            paths.push(p.to_string_lossy().into_owned());
        }
        let ev = run(home.path(), jobs, &request(&paths));

        let steps: Vec<u64> = ev
            .iter()
            .filter(|e| e["event"] == "step")
            .map(|e| e["done"].as_u64().unwrap())
            .collect();
        assert_eq!(steps.len(), 30, "one step per file ({jobs} jobs)");
        assert!(
            steps.windows(2).all(|w| w[0] < w[1]),
            "monotonic: {steps:?}"
        );
        assert_eq!(*steps.last().unwrap(), 30);

        let outputs: Vec<String> = ev
            .iter()
            .filter(|e| e["event"] == "data" && e["name"] == "outputs")
            .map(|e| e["data"]["paths"][0].as_str().unwrap().to_owned())
            .collect();
        let expected: Vec<String> = (0..30)
            .filter(|i| *i != 7)
            .map(|i| {
                d.path()
                    .join(format!("f{i:02}_pretty.json"))
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(
            outputs, expected,
            "created files in input order ({jobs} jobs)"
        );
        for o in &outputs {
            assert!(Path::new(o).is_file());
        }
        assert_eq!(ev.iter().filter(|e| e["event"] == "error").count(), 1);
        assert_eq!(ev.last().unwrap()["outcome"], "failed");
        shapes.push(shape(&ev, d.path()));
    }
    assert_eq!(
        shapes[0], shapes[1],
        "identical event stream, serial or parallel"
    );
}

#[test]
fn jobs_option_overrides_the_environment() {
    let d = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let paths: Vec<String> = (0..4)
        .map(|i| {
            let p = d.path().join(format!("g{i}.json"));
            std::fs::write(&p, "{\"a\":1}").unwrap();
            p.to_string_lossy().into_owned()
        })
        .collect();
    let req =
        serde_json::json!({"op":"run","task":"json.pretty","paths":paths,"options":{"jobs":3}});
    let ev = run(home.path(), "1", &req.to_string());
    assert_eq!(ev.last().unwrap()["outcome"], "succeeded");
    assert_eq!(ev.iter().filter(|e| e["event"] == "data").count(), 4);
}
