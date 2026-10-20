//! Task layer: turns a request (`task` id + input paths) into files on disk.
//! Currently the text family; the rest arrives phase by phase.
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use crate::convert;
use crate::paths;
use crate::settings::{Settings, TextEncodingChoice};
use crate::text::{self, LineEnding, TextError};
use crate::workspace::{commit_new, WorkingFile};

/// Input size cap for whole-file text tasks (the file is held in memory).
pub const MAX_TEXT_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug)]
pub enum TaskError {
    Unsupported,
    NotAFile,
    TooLarge,
    Text(TextError),
    Io(io::Error),
}

impl TaskError {
    pub fn key(&self) -> &'static str {
        match self {
            Self::Unsupported => "task.unimplemented",
            Self::NotAFile => "msg.notAFile",
            Self::TooLarge => "msg.fileTooLarge",
            Self::Text(e) => e.key(),
            Self::Io(_) => "msg.ioError",
        }
    }
}

impl From<io::Error> for TaskError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<TextError> for TaskError {
    fn from(e: TextError) -> Self {
        Self::Text(e)
    }
}

/// `text.encode?to=SJIS` -> (`text.encode`, {to: SJIS})
pub fn parse_task(task: &str) -> (String, HashMap<String, String>) {
    let (name, query) = task.split_once('?').unwrap_or((task, ""));
    let params = query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            p.split_once('=')
                .map_or((p.to_owned(), String::new()), |(k, v)| {
                    (k.to_owned(), v.to_owned())
                })
        })
        .collect();
    (name.to_owned(), params)
}

#[derive(Debug)]
pub struct Done {
    pub output: PathBuf,
    /// (message key, args) for the UI to localize.
    pub warnings: Vec<(&'static str, Vec<String>)>,
    /// Informational messages (key, args).
    pub infos: Vec<(&'static str, Vec<String>)>,
}

pub fn is_text_task(name: &str) -> bool {
    matches!(
        name,
        "text.encode" | "text.lineEndings" | "json.pretty" | "json.sort"
    )
}

fn read_limited(path: &Path) -> Result<Vec<u8>, TaskError> {
    let file = File::open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(TaskError::NotAFile);
    }
    if meta.len() > MAX_TEXT_BYTES {
        return Err(TaskError::TooLarge);
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.take(MAX_TEXT_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_TEXT_BYTES {
        return Err(TaskError::TooLarge);
    }
    Ok(bytes)
}

/// Write `bytes` beside `source` under a derived, never-overwriting name.
fn write_result(
    source: &Path,
    suffix: &str,
    extension: Option<&str>,
    bytes: &[u8],
) -> Result<PathBuf, TaskError> {
    for _ in 0..16 {
        let target = paths::derive(source, suffix, extension);
        let temp = paths::scratch(&target, extension.unwrap_or(""));
        let mut guard = WorkingFile::new(temp.clone());
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        match commit_new(&temp, &target) {
            Ok(()) => {
                guard.keep();
                return Ok(target);
            }
            // Someone took the name between derive and commit: pick the next one.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(TaskError::Io(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free output name",
    )))
}

pub fn run_text(
    name: &str,
    params: &HashMap<String, String>,
    input: &Path,
    settings: &Settings,
) -> Result<Done, TaskError> {
    let to = params.get("to").map(|s| s.to_uppercase());
    let bytes = read_limited(input)?;
    let file_name = input
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut warnings = Vec::new();
    let mut infos = Vec::new();

    let output = match name {
        "text.encode" => {
            let decoded = text::decode(&bytes);
            let (choice, suffix) = match to.as_deref() {
                Some("UTF8BOM") => (TextEncodingChoice::Utf8Bom, "_utf8bom"),
                Some("UTF16") => (TextEncodingChoice::Utf16, "_utf16"),
                Some("SJIS") => (TextEncodingChoice::ShiftJis, "_sjis"),
                _ => (TextEncodingChoice::Utf8, "_utf8"),
            };
            let ts = &settings.text;
            let encoded =
                text::encode(&decoded.text, choice, ts.sjis_fallback, &ts.sjis_substitute)?;
            if encoded.replaced {
                warnings.push(("msg.sjisReplaced", vec![file_name]));
            }
            write_result(input, suffix, None, &encoded.bytes)?
        }
        "text.lineEndings" => {
            let decoded = text::decode(&bytes);
            let (ending, suffix) = if to.as_deref() == Some("LF") {
                (LineEnding::Lf, "_lf")
            } else {
                (LineEnding::Crlf, "_crlf")
            };
            let converted = text::convert_line_endings(&decoded.text, ending);
            write_result(
                input,
                suffix,
                None,
                &text::encode_as_detected(&converted, decoded.encoding),
            )?
        }
        "json.pretty" | "json.sort" => {
            let decoded = text::decode(&bytes);
            let value = text::parse_json(&decoded.text)?;
            let sort = name == "json.sort";
            let value = if sort {
                text::sort_json(&value, &settings.text.json_sort)
            } else {
                value
            };
            write_result(
                input,
                if sort { "_sorted" } else { "_pretty" },
                None,
                text::pretty_json(&value).as_bytes(),
            )?
        }
        "text.convert" => {
            let target = to.as_deref().unwrap_or("JSON");
            let decoded = text::decode(&bytes);
            let converted = if target == "TXT" {
                decoded.text
            } else {
                let source = input
                    .extension()
                    .map(|e| e.to_string_lossy().to_uppercase())
                    .unwrap_or_default();
                let parsed = convert::parse(&source, &decoded.text)?;
                if let Some(d) = parsed.delimiter {
                    let shown = if d == '\t' {
                        "msg.separator.tab".to_owned()
                    } else {
                        d.to_string()
                    };
                    infos.push(("msg.readAsSeparated", vec![file_name, shown]));
                }
                convert::render(&parsed.tree, target)?
            };
            // CSV gets a BOM so spreadsheets read UTF-8 correctly; everything else is plain UTF-8.
            let bytes = if target == "CSV" {
                [&[0xEF, 0xBB, 0xBF][..], converted.as_bytes()].concat()
            } else {
                converted.into_bytes()
            };
            write_result(input, "", Some(&target.to_lowercase()), &bytes)?
        }
        _ => return Err(TaskError::Unsupported),
    };
    Ok(Done {
        output,
        warnings,
        infos,
    })
}

// ---------------------------------------------------------------- archives

pub fn is_archive_task(name: &str) -> bool {
    matches!(name, "archive.extract" | "folder.compress")
}

fn archive_done(output: PathBuf, report: crate::archive::Report) -> Done {
    Done {
        output,
        warnings: report
            .warnings
            .into_iter()
            .map(|(k, a)| (k, vec![a]))
            .collect(),
        infos: vec![],
    }
}

/// Extract an archive or compress a folder. `seven_zip` is the located 7zz, if any.
pub fn run_archive(
    name: &str,
    input: &Path,
    settings: &Settings,
    seven_zip: Option<&Path>,
) -> Result<Done, crate::archive::ArchiveError> {
    use crate::archive::{self, Format};
    use crate::settings::ArchiveCompressFormat as F;
    match name {
        "archive.extract" => {
            let x = archive::extract(input, archive::Limits::default(), seven_zip)?;
            Ok(archive_done(x.destination, x.report))
        }
        "folder.compress" => {
            let format = match settings.folder.archive.compress_format {
                F::Zip => Format::Zip,
                F::SevenZip => Format::SevenZip,
                F::Tar => Format::Tar,
                F::TarGz => Format::TarGz,
            };
            let (out, report) = archive::compress_folder(input, format, seven_zip)?;
            Ok(archive_done(out, report))
        }
        _ => Err(archive::ArchiveError::Unsupported(name.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_task_ids() {
        let (n, p) = parse_task("text.encode?to=SJIS&x");
        assert_eq!(n, "text.encode");
        assert_eq!((p["to"].as_str(), p["x"].as_str()), ("SJIS", ""));
        assert_eq!(parse_task("json.sort").1.len(), 0);
    }

    #[test]
    fn encode_writes_beside_source_without_overwriting() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("メモ.txt");
        fs::write(&src, "日本語のメモ\r\n").unwrap();
        let s = Settings::default();
        let params = parse_task("text.encode?to=SJIS").1;
        let a = run_text("text.encode", &params, &src, &s).unwrap();
        let b = run_text("text.encode", &params, &src, &s).unwrap();
        assert_eq!(a.output, d.path().join("メモ_sjis.txt"));
        assert_eq!(b.output, d.path().join("メモ_sjis (2).txt"));
        assert_eq!(
            text::decode(&fs::read(&a.output).unwrap()).text,
            "日本語のメモ\r\n"
        );
        assert_eq!(fs::read(&src).unwrap(), "日本語のメモ\r\n".as_bytes()); // original untouched
                                                                            // no scratch leftovers
        assert_eq!(fs::read_dir(d.path()).unwrap().count(), 3);
    }

    #[test]
    fn sjis_warning_and_failure_policy() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("e.txt");
        fs::write(&src, "ok😀").unwrap();
        let params = parse_task("text.encode?to=SJIS").1;
        let mut s = Settings::default();
        let done = run_text("text.encode", &params, &src, &s).unwrap();
        assert_eq!(done.warnings[0].0, "msg.sjisReplaced");
        s.text.sjis_fallback = crate::settings::SjisFallback::Fail;
        let err = run_text("text.encode", &params, &src, &s).unwrap_err();
        assert_eq!(err.key(), "msg.sjisFailed");
        assert_eq!(fs::read_dir(d.path()).unwrap().count(), 2); // failure left nothing behind
    }

    #[test]
    fn line_endings_keep_the_original_encoding() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("w.txt");
        fs::write(&src, b"\xEF\xBB\xBFa\nb\n").unwrap();
        let done = run_text(
            "text.lineEndings",
            &parse_task("text.lineEndings?to=CRLF").1,
            &src,
            &Settings::default(),
        )
        .unwrap();
        assert_eq!(fs::read(done.output).unwrap(), b"\xEF\xBB\xBFa\r\nb\r\n");
    }

    #[test]
    fn json_tasks() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("d.json");
        fs::write(&src, "{\"b\":1,\"a\":[1,2,],}").unwrap();
        let s = Settings::default();
        let p = run_text("json.pretty", &HashMap::new(), &src, &s).unwrap();
        assert!(fs::read_to_string(&p.output)
            .unwrap()
            .starts_with("{\n  \"b\": 1,"));
        let q = run_text("json.sort", &HashMap::new(), &src, &s).unwrap();
        assert!(fs::read_to_string(&q.output)
            .unwrap()
            .starts_with("{\n  \"a\": ["));
        fs::write(&src, "{broken").unwrap();
        assert_eq!(
            run_text("json.sort", &HashMap::new(), &src, &s)
                .unwrap_err()
                .key(),
            "msg.invalidJson"
        );
    }

    #[test]
    fn convert_csv_to_json_and_back_to_csv_with_bom() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("data.csv");
        fs::write(&src, "名前;数\n花;1\n").unwrap();
        let s = Settings::default();
        let j = run_text(
            "text.convert",
            &parse_task("text.convert?to=JSON").1,
            &src,
            &s,
        )
        .unwrap();
        assert_eq!(j.output, d.path().join("data.json"));
        assert_eq!(j.infos[0].0, "msg.readAsSeparated");
        assert!(fs::read_to_string(&j.output)
            .unwrap()
            .contains("\"名前\": \"花\""));
        let c = run_text(
            "text.convert",
            &parse_task("text.convert?to=CSV").1,
            &j.output,
            &s,
        )
        .unwrap();
        assert_eq!(
            fs::read(&c.output).unwrap(),
            "\u{feff}名前,数\n花,1\n".as_bytes()
        );
        assert_eq!(c.output, d.path().join("data (2).csv"));
        // same-format conversion never overwrites the source
        assert_eq!(fs::read_to_string(&src).unwrap(), "名前;数\n花;1\n");
    }

    #[test]
    fn convert_failures_leave_nothing() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("a.json");
        fs::write(&src, "{\"x\":1}").unwrap();
        let err = run_text(
            "text.convert",
            &parse_task("text.convert?to=CSV").1,
            &src,
            &Settings::default(),
        )
        .unwrap_err();
        assert_eq!(err.key(), "msg.notATable");
        assert_eq!(fs::read_dir(d.path()).unwrap().count(), 1);
        let bin = d.path().join("a.exe");
        fs::write(&bin, "x").unwrap();
        assert_eq!(
            run_text("text.convert", &HashMap::new(), &bin, &Settings::default())
                .unwrap_err()
                .key(),
            "msg.cannotConvertFrom"
        );
    }

    #[test]
    fn rejects_directories_and_unknown_tasks() {
        let d = tempfile::tempdir().unwrap();
        assert!(matches!(
            run_text(
                "json.pretty",
                &HashMap::new(),
                d.path(),
                &Settings::default()
            ),
            Err(TaskError::NotAFile)
        ));
        assert!(matches!(
            run_text("nope", &HashMap::new(), d.path(), &Settings::default()),
            Err(TaskError::NotAFile | TaskError::Unsupported)
        ));
    }
}
