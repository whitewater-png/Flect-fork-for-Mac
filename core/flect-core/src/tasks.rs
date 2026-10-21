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
    /// A malformed or repeated option name (see [`check_task`]).
    BadOption,
    NotAFile,
    TooLarge,
    Text(TextError),
    Archive(crate::archive::ArchiveError),
    Image(crate::imaging::ImageError),
    Media(crate::media::MediaError),
    Jar(crate::jar::JarError),
    Io(io::Error),
}

impl TaskError {
    pub fn key(&self) -> &'static str {
        match self {
            Self::Unsupported => "task.unimplemented",
            Self::BadOption => "task.badOption",
            Self::NotAFile => "msg.notAFile",
            Self::TooLarge => "msg.fileTooLarge",
            Self::Text(e) => e.key(),
            Self::Archive(e) => e.key(),
            Self::Image(e) => e.key(),
            Self::Media(e) => e.key(),
            Self::Jar(e) => e.key(),
            Self::Io(e) => crate::paths::io_error_key(e),
        }
    }
}

impl TaskError {
    /// Arguments for the message: normally the file name, but errors about the speech model name that instead.
    pub fn args(&self, shown: String) -> Vec<String> {
        match self {
            Self::Media(
                crate::media::MediaError::NoModel(m) | crate::media::MediaError::ModelMismatch(m),
            ) => vec![m.clone()],
            _ => vec![shown],
        }
    }
}

impl From<io::Error> for TaskError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<crate::archive::ArchiveError> for TaskError {
    fn from(e: crate::archive::ArchiveError) -> Self {
        Self::Archive(e)
    }
}
impl From<crate::jar::JarError> for TaskError {
    fn from(e: crate::jar::JarError) -> Self {
        Self::Jar(e)
    }
}
impl From<crate::media::MediaError> for TaskError {
    fn from(e: crate::media::MediaError) -> Self {
        Self::Media(e)
    }
}
impl From<crate::imaging::ImageError> for TaskError {
    fn from(e: crate::imaging::ImageError) -> Self {
        Self::Image(e)
    }
}
impl From<TextError> for TaskError {
    fn from(e: TextError) -> Self {
        Self::Text(e)
    }
}

/// An option name as the engine accepts it: `^[A-Za-z][A-Za-z0-9_]*$`, taken literally. Names are
/// never percent-decoded, so `%63onfirm` is not `confirm` (only values are decoded).
pub fn option_name_ok(k: &str) -> bool {
    let mut b = k.bytes();
    b.next().is_some_and(|c| c.is_ascii_alphabetic())
        && b.all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

/// The task's option names, literally, in order (empty parts are skipped).
fn option_names(query: &str) -> impl Iterator<Item = &str> {
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| p.split_once('=').map_or(p, |(k, _)| k))
}

/// Reject a task whose options cannot be read unambiguously: a malformed name (encoded, spaces,
/// `+`, leading digit...) or the same name twice (no "last one wins"). Checked by the engine before
/// it runs anything, so a guard that inspects the names (the CLI's) sees what the engine sees.
pub fn check_task(task: &str) -> Result<(), TaskError> {
    let query = task.split_once('?').map_or("", |(_, q)| q);
    let mut seen = std::collections::HashSet::new();
    for k in option_names(query) {
        if !option_name_ok(k) || !seen.insert(k) {
            return Err(TaskError::BadOption);
        }
    }
    Ok(())
}

/// `text.encode?to=SJIS` -> (`text.encode`, {to: SJIS}). Values are percent-decoded, names are
/// not; options with a malformed name are dropped here (callers reject such a task with
/// [`check_task`] first). Repeated names keep the first value.
pub fn parse_task(task: &str) -> (String, HashMap<String, String>) {
    let (name, query) = task.split_once('?').unwrap_or((task, ""));
    let mut params = HashMap::new();
    for p in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = p.split_once('=').unwrap_or((p, ""));
        if option_name_ok(k) {
            params
                .entry(k.to_owned())
                .or_insert_with(|| percent_decode(v));
        }
    }
    (name.to_owned(), params)
}

/// `%E6%97%A5` -> `日`. Invalid escapes are kept literally; invalid UTF-8 is replaced.
pub fn percent_decode(s: &str) -> String {
    fn hex(b: u8) -> Option<u8> {
        (b as char).to_digit(16).map(|d| d as u8)
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
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
        "text.encode" | "text.lineEndings" | "text.convert" | "json.pretty" | "json.sort"
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

/// Write `bytes` under a derived, never-overwriting name: beside `source`, or in the
/// folder `folder` (the feature's output setting) names, which is created if missing.
pub(crate) fn write_result(
    source: &Path,
    folder: &str,
    suffix: &str,
    extension: Option<&str>,
    bytes: &[u8],
) -> Result<PathBuf, TaskError> {
    let dir = paths::prepare_output_dir(source, folder)?;
    // Each lost race means another writer took that name, so this only runs out when dozens collide.
    for _ in 0..64 {
        let target = paths::derive_in(&dir, source, suffix, extension);
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
            write_result(input, &settings.output.text, suffix, None, &encoded.bytes)?
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
                &settings.output.text,
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
                &settings.output.text,
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
            write_result(
                input,
                &settings.output.text,
                "",
                Some(&target.to_lowercase()),
                &bytes,
            )?
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
            let x = archive::extract_to(
                input,
                archive::Limits::default(),
                seven_zip,
                &settings.output.archive_extract,
            )?;
            Ok(archive_done(x.destination, x.report))
        }
        "folder.compress" => {
            let format = match settings.folder.archive.compress_format {
                F::Zip => Format::Zip,
                F::SevenZip => Format::SevenZip,
                F::Tar => Format::Tar,
                F::TarGz => Format::TarGz,
            };
            let (out, report) = archive::compress_folder_to(
                input,
                format,
                seven_zip,
                &settings.output.archive_compress,
            )?;
            Ok(archive_done(out, report))
        }
        _ => Err(archive::ArchiveError::Unsupported(name.to_owned())),
    }
}

// ---------------------------------------------------------------- images

pub fn is_image_task(name: &str) -> bool {
    matches!(
        name,
        "image.resize"
            | "image.rotate"
            | "image.convert"
            | "image.compress"
            | "image.strip"
            | "image.chromakey"
    )
}

fn param_f64(params: &HashMap<String, String>, key: &str) -> Option<f64> {
    params
        .get(key)
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| v.is_finite())
}

fn lossy_quality(format: crate::imaging::Format, settings: &Settings) -> u8 {
    match format {
        crate::imaging::Format::WebP => settings.image.webp_quality.clamp(1, 100) as u8,
        _ => 92,
    }
}

/// Image tasks. Output always lands beside the source under a fresh name.
pub fn run_image(
    name: &str,
    params: &HashMap<String, String>,
    input: &Path,
    settings: &Settings,
    cancelled: &dyn Fn() -> bool,
) -> Result<Done, TaskError> {
    use crate::imaging::{self as im, Format, Frm};
    let file_name = input
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext_hint = input.extension().and_then(|e| e.to_str());
    let (mut warnings, mut infos) = (Vec::new(), Vec::new());
    let output = match name {
        "image.strip" => {
            let bytes = read_limited_image(input)?;
            let m = &settings.image.metadata;
            let opts = crate::metadata::StripOptions {
                exif: m.remove_exif,
                gps: m.remove_gps,
                comments: m.remove_comments,
                color_profile: m.remove_color_profile,
            };
            let (out, format) = im::strip(&bytes, ext_hint, opts, m.apply_orientation, 92)?;
            let data = match out {
                im::Stripped::Lossless(d) | im::Stripped::Reencoded(d) => d,
            };
            write_result(
                input,
                &settings.output.image,
                "_clean",
                Some(format.extension()),
                &data,
            )?
        }
        "image.compress" => {
            let target = params
                .get("bytes")
                .and_then(|v| v.parse::<u64>().ok())
                .filter(|v| *v > 0)
                .ok_or(TaskError::Unsupported)?;
            let size = fs::metadata(input)?.len();
            if size <= target {
                infos.push(("msg.alreadySmall", vec![file_name, size.to_string()]));
                return Ok(Done {
                    output: input.to_path_buf(),
                    warnings,
                    infos,
                });
            }
            let loaded = im::load(input)?;
            let sq = im::squeeze(
                loaded.still(),
                loaded.format,
                target,
                settings.image.allow_webp_fallback,
                cancelled,
            )?;
            if cancelled() {
                return Err(TaskError::Io(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "cancelled",
                )));
            }
            if sq.format == Format::WebP && loaded.format != Format::WebP {
                infos.push((
                    "msg.webpFallback",
                    vec![file_name.clone(), target.to_string()],
                ));
            }
            if !sq.reached {
                warnings.push((
                    "msg.targetMissed",
                    vec![file_name, target.to_string(), sq.data.len().to_string()],
                ));
            }
            write_result(
                input,
                &settings.output.image,
                "_compressed",
                Some(sq.format.extension()),
                &sq.data,
            )?
        }
        _ => {
            let loaded = im::load(input)?;
            // Same format out as in (SVG sources become PNG).
            let native = if loaded.format == Format::Svg {
                Format::Png
            } else {
                loaded.format
            };
            let (suffix, format, frames): (&str, Format, Vec<Frm>) = match name {
                "image.resize" => {
                    let to = if params
                        .get("scale")
                        .is_some_and(|s| s.eq_ignore_ascii_case("custom"))
                    {
                        im::ResizeTo::Pixels {
                            width: param_f64(params, "w").unwrap_or(0.0).clamp(0.0, 32768.0) as u32,
                            height: param_f64(params, "h").unwrap_or(0.0).clamp(0.0, 32768.0)
                                as u32,
                            keep_aspect: params.get("keep").is_none_or(|v| v != "0"),
                        }
                    } else {
                        im::ResizeTo::Percent(
                            param_f64(params, "scale")
                                .filter(|v| *v > 0.0)
                                .unwrap_or(50.0),
                        )
                    };
                    let frames = loaded
                        .frames
                        .iter()
                        .map(|f| {
                            Ok(Frm {
                                img: im::resize(&f.img, to)?,
                                delay_ms: f.delay_ms,
                            })
                        })
                        .collect::<Result<Vec<_>, im::ImageError>>()?;
                    ("_resized", native, frames)
                }
                "image.rotate" => {
                    let deg = param_f64(params, "deg").unwrap_or(90.0);
                    let opaque = !native.has_alpha();
                    let frames = loaded
                        .frames
                        .iter()
                        .map(|f| {
                            Ok(Frm {
                                img: im::rotate(&f.img, deg, opaque)?,
                                delay_ms: f.delay_ms,
                            })
                        })
                        .collect::<Result<Vec<_>, im::ImageError>>()?;
                    ("_rotated", native, frames)
                }
                "image.convert" => {
                    let target = params
                        .get("to")
                        .and_then(|t| Format::from_name(t))
                        .unwrap_or(Format::Png);
                    let frames = if target == Format::Gif {
                        loaded.frames.clone()
                    } else {
                        vec![Frm {
                            img: loaded.still().clone(),
                            delay_ms: 0,
                        }]
                    };
                    ("", target, frames)
                }
                "image.chromakey" => {
                    let key = im::parse_hex_color(&settings.image.background_key_color)
                        .unwrap_or([0, 255, 0]);
                    let keyed =
                        im::chroma_key(loaded.still(), key, settings.image.background_tolerance);
                    (
                        "_keyed",
                        Format::Png,
                        vec![Frm {
                            img: keyed,
                            delay_ms: 0,
                        }],
                    )
                }
                _ => return Err(TaskError::Unsupported),
            };
            let quality = lossy_quality(format, settings);
            let data = im::encode_frames(&frames, format, quality)?;
            write_result(
                input,
                &settings.output.image,
                suffix,
                Some(format.extension()),
                &data,
            )?
        }
    };
    Ok(Done {
        output,
        warnings,
        infos,
    })
}

fn read_limited_image(path: &Path) -> Result<Vec<u8>, TaskError> {
    let meta = fs::metadata(path)?;
    if !meta.is_file() {
        return Err(TaskError::NotAFile);
    }
    if meta.len() > crate::imaging::MAX_INPUT_BYTES {
        return Err(TaskError::TooLarge);
    }
    Ok(fs::read(path)?)
}

// ---------------------------------------------------------------- dispatch

/// Everything a single-file task needs besides its input.
pub struct Ctx<'a> {
    pub settings: &'a Settings,
    pub seven_zip: Option<&'a Path>,
    pub progress: &'a dyn crate::folder::Progress,
    /// ffmpeg and friends, when installed.
    pub media: Option<&'a crate::mediatasks::MediaEnv>,
    /// A real JDK and the pinned CFR jar, for `jar.decompile`.
    pub java: Option<&'a Path>,
    pub cfr: Option<&'a Path>,
}

pub fn is_file_task(name: &str) -> bool {
    is_text_task(name)
        || name == "jar.decompile"
        || is_archive_task(name)
        || is_image_task(name)
        || crate::mediatasks::is_media_task(name)
        || crate::privacy::is_task(name)
}

/// Run one single-file task.
pub fn run(task: &str, input: &Path, ctx: &Ctx<'_>) -> Result<Done, TaskError> {
    check_task(task)?;
    let (name, params) = parse_task(task);
    if is_text_task(&name) {
        run_text(&name, &params, input, ctx.settings)
    } else if is_archive_task(&name) {
        Ok(run_archive(&name, input, ctx.settings, ctx.seven_zip)?)
    } else if is_image_task(&name) {
        run_image(&name, &params, input, ctx.settings, &|| {
            ctx.progress.cancelled()
        })
    } else if crate::privacy::is_task(&name) {
        crate::privacy::run_file(&name, &params, input, ctx.settings)
    } else if crate::mediatasks::is_media_task(&name) {
        let env = ctx.media.ok_or(crate::media::MediaError::NoTools)?;
        let job = crate::mediatasks::Job::new(env, ctx.settings, ctx.progress);
        Ok(crate::mediatasks::run(&name, &params, input, &job)?)
    } else if name == "jar.decompile" {
        let java = ctx.java.ok_or(crate::jar::JarError::NoJava)?;
        let cfr = ctx.cfr.ok_or(crate::jar::JarError::NoCfr)?;
        let root = Some(ctx.settings.code.decompile.output_path.trim())
            .filter(|r| !r.is_empty())
            .map(Path::new);
        let sandbox = Some(Path::new("/usr/bin/sandbox-exec")).filter(|p| p.is_file());
        let out = crate::jar::decompile(input, java, cfr, root, sandbox, &|| {
            ctx.progress.cancelled()
        })?;
        let mut warnings: Vec<_> = out
            .report
            .warnings
            .into_iter()
            .map(|(k, a)| (k, vec![a]))
            .collect();
        if out.unsandboxed {
            warnings.push(("msg.sandboxUnavailable", vec![]));
        }
        Ok(Done {
            output: out.destination,
            warnings,
            infos: vec![],
        })
    } else {
        Err(TaskError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The engine answers "unimplemented" to any task `is_file_task` does not list, which made JAR decompiling unreachable.
    #[test]
    fn jar_decompile_is_dispatched_as_a_file_task() {
        assert!(is_file_task("jar.decompile"));
    }

    /// `run_text` implements `text.convert`, but the dispatcher did not list it, so the engine answered "unimplemented".
    #[test]
    fn text_convert_is_dispatched() {
        assert!(is_file_task("text.convert"));
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("t.csv");
        fs::write(&src, "a,b\n1,2\n").unwrap();
        let ctx = Ctx {
            settings: &Settings::default(),
            seven_zip: None,
            progress: &crate::folder::NullProgress,
            media: None,
            java: None,
            cfr: None,
        };
        let done = run("text.convert?to=JSON", &src, &ctx).unwrap();
        assert_eq!(done.output, d.path().join("t.json"));
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("%E6%97%A5%E6%9C%AC"), "日本");
        assert_eq!(percent_decode("a%2Fb%20c"), "a/b c");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz%4"), "%zz%4");
        assert_eq!(percent_decode("%é"), "%é");
        assert_eq!(percent_decode("%FF"), "\u{fffd}");
        assert_eq!(
            parse_task("folder.rename?prefix=%E6%97%85_&name=a%26b").1["name"],
            "a&b"
        );
    }

    #[test]
    fn option_names_are_literal_and_strict() {
        for bad in [
            "folder.removeEmpty?%63onfirm=1",
            "folder.removeEmpty?%43ONFIRM=1",
            "folder.removeEmpty?confirm%3D1",
            "folder.removeEmpty?+confirm=1",
            "folder.removeEmpty?%20confirm=1",
            "folder.removeEmpty?confirm =1",
            "folder.removeEmpty?1confirm=1",
            "folder.removeEmpty?_confirm=1",
            "folder.removeEmpty?=1",
            "folder.removeEmpty?confirm=1&confirm=1",
            "folder.removeDuplicate?dry=1&dry=0",
            "folder.removeDuplicate?mode=all&%64ry=0",
            "folder.removeDuplicate?d%72y=0",
        ] {
            assert!(check_task(bad).is_err(), "{bad}");
            assert!(
                !parse_task(bad).1.contains_key("confirm") || bad.contains("confirm=1&confirm=1"),
                "{bad}"
            );
        }
        // what parse_task does with a task check_task refused: never decodes a name, first value wins
        assert!(parse_task("x?%63onfirm=1").1.is_empty());
        assert_eq!(parse_task("x?dry=1&dry=0").1["dry"], "1");
        for good in [
            "folder.removeEmpty?confirm=1",
            "folder.removeEmpty?confirm=%31",
            "folder.rename?prefix=%E6%97%85_&name=a%26b&dry=0",
            "image.resize?scale=custom&w=1&h=2&keep=1",
            "video.trim?start=1.000&end=2.000",
            "text.encode?to=SJIS&x",
            "json.sort",
            "json.sort?",
            "a?b_2=",
        ] {
            assert!(check_task(good).is_ok(), "{good}");
        }
        assert_eq!(
            parse_task("folder.removeEmpty?confirm=%31").1["confirm"],
            "1"
        );
        assert!(matches!(
            run(
                "text.encode?%74o=SJIS",
                Path::new("/nonexistent"),
                &Ctx {
                    settings: &Settings::default(),
                    seven_zip: None,
                    progress: &crate::folder::NullProgress,
                    media: None,
                    java: None,
                    cfr: None,
                }
            ),
            Err(TaskError::BadOption)
        ));
    }

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
    fn output_folder_setting_redirects_results_and_never_overwrites() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("memo.txt");
        fs::write(&src, "abc\r\n").unwrap();
        let mut s = Settings::default();
        s.output.text = "./Converted".into();
        let params = parse_task("text.lineEndings?to=LF").1;
        let a = run_text("text.lineEndings", &params, &src, &s).unwrap();
        assert_eq!(a.output, d.path().join("Converted/memo_lf.txt"));
        // a file already there is kept; the new one is numbered
        let b = run_text("text.lineEndings", &params, &src, &s).unwrap();
        assert_eq!(b.output, d.path().join("Converted/memo_lf (2).txt"));
        assert_eq!(fs::read(&a.output).unwrap(), b"abc\n");
        // a feature left unset still writes beside the original
        s.output.text.clear();
        let c = run_text("text.lineEndings", &params, &src, &s).unwrap();
        assert_eq!(c.output, d.path().join("memo_lf.txt"));
        // no scratch leftovers in either folder
        assert_eq!(fs::read_dir(d.path().join("Converted")).unwrap().count(), 2);
        assert_eq!(fs::read_dir(d.path()).unwrap().count(), 3);
    }

    #[test]
    fn output_folder_that_is_a_file_fails_without_touching_it() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("memo.txt");
        fs::write(&src, "abc").unwrap();
        fs::write(d.path().join("Out"), "precious").unwrap();
        let mut s = Settings::default();
        s.output.text = "Out".into();
        let params = parse_task("text.lineEndings?to=LF").1;
        assert!(run_text("text.lineEndings", &params, &src, &s).is_err());
        assert_eq!(fs::read(d.path().join("Out")).unwrap(), b"precious");
    }

    #[test]
    fn archive_tasks_honour_their_output_folders() {
        let d = tempfile::tempdir().unwrap();
        let folder = d.path().join("docs");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("a.txt"), "hello").unwrap();
        let mut s = Settings::default();
        s.output.archive_compress = "./Zips".into();
        s.output.archive_extract = d.path().join("Unpacked").to_string_lossy().into_owned();
        let packed = run_archive("folder.compress", &folder, &s, None).unwrap();
        assert_eq!(packed.output, d.path().join("Zips/docs.zip"));
        let again = run_archive("folder.compress", &folder, &s, None).unwrap();
        assert_eq!(again.output, d.path().join("Zips/docs (2).zip"));
        let x = run_archive("archive.extract", &packed.output, &s, None).unwrap();
        assert_eq!(x.output, d.path().join("Unpacked/docs"));
        assert_eq!(fs::read(x.output.join("docs/a.txt")).unwrap(), b"hello");
        let x2 = run_archive("archive.extract", &packed.output, &s, None).unwrap();
        assert_eq!(x2.output, d.path().join("Unpacked/docs (2)"));
        // no hidden scratch folders left behind
        for dir in [
            d.path().join("Zips"),
            d.path().join("Unpacked"),
            d.path().into(),
        ] {
            let hidden = fs::read_dir(dir)
                .unwrap()
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
                .count();
            assert_eq!(hidden, 0);
        }
    }

    #[test]
    fn compressing_into_the_folder_being_packed_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let folder = d.path().join("docs");
        fs::create_dir(&folder).unwrap();
        let mut s = Settings::default();
        s.output.archive_compress = folder.join("out").to_string_lossy().into_owned();
        assert!(run_archive("folder.compress", &folder, &s, None).is_err());
        assert!(!folder.join("out").exists());
    }

    #[test]
    fn output_setting_per_task() {
        let o = crate::settings::OutputSettings {
            image: "i".into(),
            video: "v".into(),
            audio: "a".into(),
            ocr: "o".into(),
            ..Default::default()
        };
        assert_eq!(o.for_task("image.resize"), "i");
        assert_eq!(o.for_task("image.ocr"), "o");
        assert_eq!(o.for_task("video.compress"), "v");
        assert_eq!(o.for_task("video.extractAudio"), "a");
        assert_eq!(o.for_task("audio.convert"), "a");
        assert_eq!(o.for_task("folder.rename"), "");
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

    mod images {
        use super::super::*;
        use crate::imaging::{self as im, Format};
        use ::image::{Rgba, RgbaImage};

        fn photo(w: u32, h: u32) -> RgbaImage {
            let mut s = 7u32;
            RgbaImage::from_fn(w, h, |x, y| {
                s = s.wrapping_mul(1664525).wrapping_add(1013904223);
                Rgba([
                    (x * 255 / w) as u8 ^ (s >> 28) as u8,
                    (y * 255 / h) as u8,
                    (s >> 20) as u8,
                    255,
                ])
            })
        }

        fn run1(task: &str, input: &Path, settings: &Settings) -> Result<Done, TaskError> {
            run(
                task,
                input,
                &Ctx {
                    settings,
                    seven_zip: None,
                    progress: &crate::folder::NullProgress,
                    media: None,
                    java: None,
                    cfr: None,
                },
            )
        }

        fn dims(path: &Path) -> (u32, u32) {
            im::load(path).unwrap().still().dimensions()
        }

        #[test]
        fn resize_percent_and_custom() {
            let d = tempfile::tempdir().unwrap();
            let src = d.path().join("写真.png");
            fs::write(&src, im::encode(&photo(200, 100), Format::Png, 0).unwrap()).unwrap();
            let s = Settings::default();
            let a = run1("image.resize?scale=50", &src, &s).unwrap();
            assert_eq!(
                (a.output.clone(), dims(&a.output)),
                (d.path().join("写真_resized.png"), (100, 50))
            );
            let b = run1("image.resize?scale=12.5", &src, &s).unwrap();
            assert_eq!(dims(&b.output), (25, 13));
            let c = run1("image.resize?scale=custom&w=60&h=0", &src, &s).unwrap();
            assert_eq!(dims(&c.output), (60, 30));
            let e = run1("image.resize?scale=custom&w=50&h=50&keep=0", &src, &s).unwrap();
            assert_eq!(dims(&e.output), (50, 50));
            assert_eq!(dims(&src), (200, 100), "original untouched");
        }

        #[test]
        fn rotate_formats_decide_the_corner_colour() {
            let d = tempfile::tempdir().unwrap();
            let png = d.path().join("a.png");
            let jpg = d.path().join("b.jpg");
            fs::write(&png, im::encode(&photo(40, 20), Format::Png, 0).unwrap()).unwrap();
            fs::write(&jpg, im::encode(&photo(40, 20), Format::Jpeg, 90).unwrap()).unwrap();
            let s = Settings::default();
            let p = run1("image.rotate?deg=45", &png, &s).unwrap();
            assert_eq!(im::load(&p.output).unwrap().still().get_pixel(0, 0)[3], 0);
            let j = run1("image.rotate?deg=45", &jpg, &s).unwrap();
            let px = *im::load(&j.output).unwrap().still().get_pixel(0, 0);
            assert!(px[0] > 240 && px[1] > 240 && px[2] > 240, "{px:?}");
            assert_eq!(j.output.extension().unwrap(), "jpg");
        }

        #[test]
        fn convert_between_formats_and_flatten_jpeg() {
            let d = tempfile::tempdir().unwrap();
            let src = d.path().join("c.png");
            let img = RgbaImage::from_pixel(30, 30, Rgba([255, 0, 0, 0])); // fully transparent red
            fs::write(&src, im::encode(&img, Format::Png, 0).unwrap()).unwrap();
            let s = Settings::default();
            for (to, ext) in [
                ("JPG", "jpg"),
                ("WEBP", "webp"),
                ("GIF", "gif"),
                ("ICO", "ico"),
                ("SVG", "svg"),
                ("PNG", "png"),
            ] {
                let r = run1(&format!("image.convert?to={to}"), &src, &s).unwrap();
                assert_eq!(r.output.extension().unwrap(), ext);
                assert!(r.output.exists());
            }
            let j = d.path().join("c.jpg");
            let px = *im::load(&j).unwrap().still().get_pixel(5, 5);
            assert!(
                px[0] > 240 && px[1] > 240,
                "transparent areas become white in JPEG: {px:?}"
            );
            assert!(
                d.path().join("c (2).png").exists(),
                "png->png never overwrites"
            );
            assert_eq!(
                run1("image.convert?to=HEIC", &src, &s).unwrap_err().key(),
                "msg.imageUnsupported"
            );
            assert_eq!(
                fs::read_dir(d.path()).unwrap().count(),
                7,
                "failed HEIC left no scratch file"
            );
        }

        #[test]
        fn compress_hits_the_target_or_says_so() {
            let d = tempfile::tempdir().unwrap();
            let src = d.path().join("big.jpg");
            let bytes = im::encode(&photo(800, 600), Format::Jpeg, 98).unwrap();
            fs::write(&src, &bytes).unwrap();
            let s = Settings::default();
            let target = bytes.len() as u64 / 5;
            let r = run1(&format!("image.compress?bytes={target}"), &src, &s).unwrap();
            assert!(fs::metadata(&r.output).unwrap().len() <= target && r.warnings.is_empty());
            assert_eq!(r.output, d.path().join("big_compressed.jpg"));
            let small = run1(
                &format!("image.compress?bytes={}", bytes.len() * 2),
                &src,
                &s,
            )
            .unwrap();
            assert_eq!(small.infos[0].0, "msg.alreadySmall");
            assert!(!d.path().join("big_compressed (2).jpg").exists());
            let miss = run1("image.compress?bytes=10", &src, &s).unwrap();
            assert_eq!(miss.warnings[0].0, "msg.targetMissed");
            assert!(matches!(
                run1("image.compress", &src, &s),
                Err(TaskError::Unsupported)
            ));
        }

        #[test]
        fn strip_and_chromakey() {
            let d = tempfile::tempdir().unwrap();
            let src = d.path().join("p.jpg");
            fs::write(&src, im::encode(&photo(32, 32), Format::Jpeg, 90).unwrap()).unwrap();
            let s = Settings::default();
            let r = run1("image.strip", &src, &s).unwrap();
            assert_eq!(r.output, d.path().join("p_clean.jpg"));
            let g = d.path().join("g.png");
            fs::write(
                &g,
                im::encode(
                    &RgbaImage::from_pixel(4, 4, Rgba([0, 255, 0, 255])),
                    Format::Png,
                    0,
                )
                .unwrap(),
            )
            .unwrap();
            let k = run1("image.chromakey", &g, &s).unwrap();
            assert_eq!(k.output, d.path().join("g_keyed.png"));
            assert_eq!(im::load(&k.output).unwrap().still().get_pixel(1, 1)[3], 0);
        }

        #[test]
        fn broken_and_hostile_files_leave_nothing() {
            let d = tempfile::tempdir().unwrap();
            let bad = d.path().join("bad.png");
            fs::write(&bad, b"\x89PNG not really").unwrap();
            let s = Settings::default();
            for t in [
                "image.resize?scale=50",
                "image.rotate?deg=10",
                "image.convert?to=JPG",
                "image.compress?bytes=5",
                "image.strip",
                "image.chromakey",
            ] {
                assert!(run1(t, &bad, &s).is_err(), "{t}");
            }
            assert_eq!(fs::read_dir(d.path()).unwrap().count(), 1);
            assert_eq!(
                run1("image.resize", d.path(), &s).unwrap_err().key(),
                "msg.imageUnsupported"
            );
        }
    }
}
