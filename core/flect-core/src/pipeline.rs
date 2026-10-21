//! Pipelines: several single-file tasks chained in one run (resize -> convert -> strip ...).
//!
//! A step is a task string exactly as `run.task` takes it. Only tasks that turn one file into one
//! file without a dialog or an OS service can be chained, and every step is checked strictly
//! (known keys, bounded values) because steps also come from `presets.json`, which is untrusted.
//! Every step writes into a private work folder; only the last result is published beside the
//! original (or in the configured output folder), under the usual never-overwrite rule.
//! See docs/mac-port/17-presets-pipeline.ja.md.
use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::menu::{self, Category};
use crate::paths;
use crate::settings::{OutputSettings, Settings};
use crate::tasks::{self, Ctx, Done, TaskError};
use crate::workspace::{commit_new, unique_token, BRAND};

pub const MAX_STEPS: usize = 8;
pub const MAX_STEP_BYTES: usize = 1024;
const MAX_BYTES_PARAM: u64 = 1 << 40;
const MAX_PIXELS: u64 = 32768;
const ABANDONED_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// What a step reads and writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Image,
    Video,
    Audio,
    Text,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// The task string sent to the task layer (`image.convert?to=JPG`).
    pub task: String,
    /// The task id without parameters.
    pub name: &'static str,
    pub accepts: &'static [Kind],
    pub produces: Kind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    /// No steps, or more than [`MAX_STEPS`].
    StepCount,
    /// A task that cannot be chained (folder, dialog, OS service...).
    NotAllowed(String),
    /// Bad syntax, unknown / duplicate / missing parameter, value out of range.
    BadStep(String),
    /// The output of the first cannot feed the second.
    Incompatible(String, String),
}

impl PlanError {
    pub fn key(&self) -> &'static str {
        match self {
            Self::StepCount => "msg.pipelineStepCount",
            Self::NotAllowed(_) => "msg.pipelineStepNotAllowed",
            Self::BadStep(_) => "msg.pipelineBadStep",
            Self::Incompatible(..) => "msg.pipelineIncompatible",
        }
    }
    pub fn args(&self) -> Vec<String> {
        match self {
            Self::StepCount => vec![MAX_STEPS.to_string()],
            Self::NotAllowed(s) | Self::BadStep(s) => vec![s.clone()],
            Self::Incompatible(a, b) => vec![a.clone(), b.clone()],
        }
    }
}

/// A step as shown in a message: bounded, since it may come from an untrusted file.
fn shown(step: &str) -> String {
    let mut s: String = step.chars().filter(|c| !c.is_control()).take(80).collect();
    if step.chars().count() > 80 {
        s.push('…');
    }
    s
}

struct Params<'a>(Vec<(&'a str, &'a str)>);

impl Params<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
    }
    fn only(&self, allowed: &[&str]) -> bool {
        self.0.iter().all(|(k, _)| allowed.contains(k))
    }
}

/// `-12.5`, `50`: plain decimal only (no exponent, no `inf`/`nan`).
fn number(v: &str) -> Option<f64> {
    let body = v.strip_prefix('-').unwrap_or(v);
    let (int, frac) = body.split_once('.').unwrap_or((body, "0"));
    let digits = |s: &str| !s.is_empty() && s.len() <= 12 && s.bytes().all(|b| b.is_ascii_digit());
    (digits(int) && digits(frac))
        .then(|| v.parse::<f64>().ok())
        .flatten()
}

fn integer(v: &str, max: u64) -> Option<u64> {
    (!v.is_empty() && v.len() <= 15 && v.bytes().all(|b| b.is_ascii_digit()))
        .then(|| v.parse::<u64>().ok())
        .flatten()
        .filter(|n| *n <= max)
}

fn resize(p: &Params, kind: Kind) -> Option<Kind> {
    if !p.only(&["scale", "w", "h", "keep"]) {
        return None;
    }
    if p.get("scale")? == "custom" {
        let w = integer(p.get("w")?, MAX_PIXELS)?;
        let h = integer(p.get("h")?, MAX_PIXELS)?;
        let keep_ok = p.get("keep").is_none_or(|k| k == "0" || k == "1");
        (keep_ok && (w > 0 || h > 0)).then_some(kind)
    } else {
        let scale = number(p.get("scale")?)?;
        let plain = p.get("w").is_none() && p.get("h").is_none() && p.get("keep").is_none();
        (plain && scale > 0.0 && scale <= 1000.0).then_some(kind)
    }
}

fn rotate(p: &Params, kind: Kind) -> Option<Kind> {
    let deg = number(p.get("deg")?)?;
    (p.only(&["deg"]) && (-360.0..=360.0).contains(&deg)).then_some(kind)
}

fn to<'a>(p: &Params<'a>, allowed: &[&str]) -> Option<&'a str> {
    let t = p.0.iter().find(|(k, _)| *k == "to").map(|(_, v)| *v)?;
    (p.only(&["to"]) && allowed.contains(&t)).then_some(t)
}

const AUDIO: &[&str] = &["MP3", "M4A", "WAV", "AIF", "AIFF", "AAC", "OGG", "WMA"];
const IMAGE_IN: &[Kind] = &[Kind::Image];
const VIDEO_IN: &[Kind] = &[Kind::Video];
const AUDIO_IN: &[Kind] = &[Kind::Audio, Kind::Video];
const TEXT_IN: &[Kind] = &[Kind::Text];

/// The chainable tasks: (id, accepted inputs, parameter check returning the output kind).
type Check = fn(&Params) -> Option<Kind>;
const SPECS: &[(&str, &[Kind], Check)] = &[
    ("image.resize", IMAGE_IN, |p| resize(p, Kind::Image)),
    ("image.rotate", IMAGE_IN, |p| rotate(p, Kind::Image)),
    ("image.compress", IMAGE_IN, |p| {
        (p.only(&["bytes"]) && integer(p.get("bytes")?, MAX_BYTES_PARAM)? > 0)
            .then_some(Kind::Image)
    }),
    ("image.convert", IMAGE_IN, |p| {
        to(p, &["PNG", "JPG", "WEBP", "ICO", "GIF", "SVG"]).map(|_| Kind::Image)
    }),
    ("image.strip", IMAGE_IN, |p| {
        p.0.is_empty().then_some(Kind::Image)
    }),
    ("image.chromakey", IMAGE_IN, |p| {
        p.0.is_empty().then_some(Kind::Image)
    }),
    ("video.resize", VIDEO_IN, |p| resize(p, Kind::Video)),
    ("video.rotate", VIDEO_IN, |p| rotate(p, Kind::Video)),
    ("video.compress", VIDEO_IN, |p| {
        let ok = match (p.get("bytes"), p.get("preset")) {
            (Some(b), None) => integer(b, MAX_BYTES_PARAM).is_some_and(|n| n > 0),
            (None, Some(preset)) => preset == "discord",
            _ => false,
        };
        (ok && p.only(&["bytes", "preset"])).then_some(Kind::Video)
    }),
    ("video.convert", VIDEO_IN, |p| {
        match to(
            p,
            &["MP4", "MOV", "MKV", "M4A", "AVI", "WEBM", "FLV", "GIF"],
        )? {
            "M4A" => Some(Kind::Audio),
            "GIF" => Some(Kind::Image),
            _ => Some(Kind::Video),
        }
    }),
    ("video.extractAudio", VIDEO_IN, |p| {
        to(p, AUDIO).map(|_| Kind::Audio)
    }),
    ("audio.convert", AUDIO_IN, |p| {
        to(p, AUDIO).map(|_| Kind::Audio)
    }),
    ("text.convert", TEXT_IN, |p| {
        to(p, &["TXT", "JSON", "XML", "CSV", "HTML", "MD"]).map(|_| Kind::Text)
    }),
    ("text.encode", TEXT_IN, |p| {
        to(p, &["UTF8", "UTF8BOM", "UTF16", "SJIS"]).map(|_| Kind::Text)
    }),
    ("text.lineEndings", TEXT_IN, |p| {
        to(p, &["CRLF", "LF"]).map(|_| Kind::Text)
    }),
    ("json.pretty", TEXT_IN, |p| {
        p.0.is_empty().then_some(Kind::Text)
    }),
    ("json.sort", TEXT_IN, |p| {
        p.0.is_empty().then_some(Kind::Text)
    }),
];

/// Task ids a pipeline (and so a preset) may contain.
pub fn chainable_tasks() -> impl Iterator<Item = &'static str> {
    SPECS.iter().map(|(id, _, _)| *id)
}

fn key_ok(k: &str) -> bool {
    !k.is_empty() && k.len() <= 16 && k.bytes().all(|b| b.is_ascii_alphabetic())
}

fn value_ok(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 32
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
}

/// Check one step: `name` or `name?k=v&k=v`, with the parameters its task requires.
pub fn parse_step(step: &str) -> Result<Step, PlanError> {
    let bad = || PlanError::BadStep(shown(step));
    if step.is_empty() || step.len() > MAX_STEP_BYTES {
        return Err(bad());
    }
    let (name, query) = match step.split_once('?') {
        Some((n, q)) => (n, Some(q)),
        None => (step, None),
    };
    let Some(&(id, accepts, check)) = SPECS.iter().find(|(id, _, _)| *id == name) else {
        return Err(PlanError::NotAllowed(shown(step)));
    };
    let mut params = Params(Vec::new());
    if let Some(query) = query {
        if query.is_empty() {
            return Err(bad());
        }
        for pair in query.split('&') {
            let (k, v) = pair.split_once('=').ok_or_else(bad)?;
            if !key_ok(k) || !value_ok(v) || params.get(k).is_some() {
                return Err(bad());
            }
            params.0.push((k, v));
        }
    }
    let produces = check(&params).ok_or_else(bad)?;
    Ok(Step {
        task: step.to_owned(),
        name: id,
        accepts,
        produces,
    })
}

/// Check a whole chain: 1..=MAX_STEPS valid steps, each output usable by the next step.
pub fn plan<S: AsRef<str>>(steps: &[S]) -> Result<Vec<Step>, PlanError> {
    if steps.is_empty() || steps.len() > MAX_STEPS {
        return Err(PlanError::StepCount);
    }
    let parsed = steps
        .iter()
        .map(|s| parse_step(s.as_ref()))
        .collect::<Result<Vec<_>, _>>()?;
    for pair in parsed.windows(2) {
        if !pair[1].accepts.contains(&pair[0].produces) {
            return Err(PlanError::Incompatible(
                shown(&pair[0].task),
                shown(&pair[1].task),
            ));
        }
    }
    Ok(parsed)
}

/// The kind of an input file, by the same rules as the menus (`None` for folders and unknown types).
pub fn input_kind(path: &Path) -> Option<Kind> {
    let is_dir = fs::metadata(path).ok()?.is_dir();
    match menu::classify(path, is_dir)?.category {
        Category::Image => Some(Kind::Image),
        Category::Video => Some(Kind::Video),
        Category::Audio => Some(Kind::Audio),
        Category::Text => Some(Kind::Text),
        _ => None,
    }
}

// ---------------------------------------------------------------- private work folder

const WORK_PREFIX: &str = "-pipeline-";

/// `$TMPDIR/Flect-for-Mac-pipeline-<unique>`: created exclusively with mode 0700, removed on drop
/// (success, failure, cancel or unwinding).
pub struct WorkDir {
    root: PathBuf,
}

impl WorkDir {
    pub fn create(parent: &Path) -> io::Result<Self> {
        let root = parent.join(format!("{BRAND}{WORK_PREFIX}{}", unique_token()));
        // `create`, not `create_all`: fails if anything (a folder, a planted symlink) already has the name.
        fs::DirBuilder::new().mode(0o700).create(&root)?;
        let meta = fs::symlink_metadata(&root)?;
        if !meta.is_dir() || meta.mode() & 0o077 != 0 {
            let _ = fs::remove_dir(&root);
            return Err(io::Error::other("work folder is not private"));
        }
        sweep_abandoned(parent, &root, meta.uid());
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// A private folder for one input file, removed when the guard is dropped.
    fn file_dir(&self, index: usize) -> io::Result<FileDir> {
        let path = self.root.join(format!("f{index}"));
        fs::DirBuilder::new().mode(0o700).create(&path)?;
        Ok(FileDir { path })
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        // remove_dir_all does not follow symlinks inside the tree.
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct FileDir {
    path: PathBuf,
}

impl Drop for FileDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Work folders left by a killed engine: same prefix, a real folder owned by us, older than a day.
fn sweep_abandoned(parent: &Path, keep: &Path, uid: u32) {
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    let prefix = format!("{BRAND}{WORK_PREFIX}");
    for entry in entries.flatten() {
        let path = entry.path();
        if path == keep || !entry.file_name().to_string_lossy().starts_with(&prefix) {
            continue;
        }
        let Ok(meta) = path.symlink_metadata() else {
            continue;
        };
        let old = meta
            .modified()
            .ok()
            .and_then(|m| SystemTime::now().duration_since(m).ok())
            .is_some_and(|age| age > ABANDONED_AFTER);
        if meta.is_dir() && meta.uid() == uid && old {
            let _ = fs::remove_dir_all(&path);
        }
    }
}

// ---------------------------------------------------------------- running

#[derive(Debug)]
pub enum RunError {
    /// The file is not what the first step reads.
    WrongInput,
    Task(TaskError),
}

impl RunError {
    pub fn key(&self) -> &'static str {
        match self {
            Self::WrongInput => "msg.pipelineWrongInput",
            Self::Task(e) => e.key(),
        }
    }
    pub fn args(&self, shown: String) -> Vec<String> {
        match self {
            Self::WrongInput => vec![shown],
            Self::Task(e) => e.args(shown),
        }
    }
}

impl From<TaskError> for RunError {
    fn from(e: TaskError) -> Self {
        Self::Task(e)
    }
}

impl From<io::Error> for RunError {
    fn from(e: io::Error) -> Self {
        Self::Task(TaskError::Io(e))
    }
}

/// The output folder setting the task layer uses for `task` (JSON tasks write with the text setting).
fn folder_setting<'a>(output: &'a OutputSettings, task: &str) -> &'a str {
    if task.starts_with("json.") {
        &output.text
    } else {
        output.for_task(task)
    }
}

/// `settings` with every output folder pointing at `dir` (absolute), so any step writes there.
fn writing_into(settings: &Settings, dir: &Path) -> io::Result<Settings> {
    let text = dir
        .to_str()
        .ok_or_else(|| io::Error::other("work folder path is not UTF-8"))?;
    // The task layer trims and cleans the setting; make sure it still names exactly `dir`.
    if paths::output_dir(dir, text) != dir {
        return Err(io::Error::other("work folder path does not round-trip"));
    }
    let mut s = settings.clone();
    s.output = OutputSettings {
        image: text.into(),
        video: text.into(),
        audio: text.into(),
        text: text.into(),
        pdf: text.into(),
        ocr: text.into(),
        archive_extract: text.into(),
        archive_compress: text.into(),
        report: text.into(),
    };
    Ok(s)
}

fn cancelled() -> RunError {
    RunError::Task(TaskError::Io(io::Error::new(
        io::ErrorKind::Interrupted,
        "cancelled",
    )))
}

/// Run the chain on one file. Intermediate results stay in a private folder under `work`; the
/// last one is published beside `input` (or in the last task's output folder) under a fresh name.
/// `Done.output == input` means nothing was created (every step left its input as it was).
pub fn run_file(
    steps: &[Step],
    input: &Path,
    ctx: &Ctx<'_>,
    work: &WorkDir,
    index: usize,
) -> Result<Done, RunError> {
    let (first, last) = match (steps.first(), steps.last()) {
        (Some(f), Some(l)) => (f, l),
        _ => return Err(TaskError::Unsupported.into()),
    };
    match input_kind(input) {
        Some(k) if first.accepts.contains(&k) => {}
        _ => return Err(RunError::WrongInput),
    }
    let dir = work.file_dir(index)?;
    let settings = writing_into(ctx.settings, &dir.path)?;
    let stage = Ctx {
        settings: &settings,
        ..*ctx
    };
    let total = steps.len().to_string();
    let (mut warnings, mut infos) = (Vec::new(), Vec::new());
    let mut current = input.to_path_buf();
    for (i, step) in steps.iter().enumerate() {
        if ctx.progress.cancelled() {
            return Err(cancelled());
        }
        ctx.progress
            .status("msg.pipelineStage", &[(i + 1).to_string(), total.clone()]);
        let done = tasks::run(&step.task, &current, &stage)?;
        warnings.extend(done.warnings);
        infos.extend(done.infos);
        // A step either left its input as is or wrote into the work folder; anything else is a bug.
        if done.output != current && !done.output.starts_with(&dir.path) {
            return Err(io::Error::other("a step wrote outside the work folder").into());
        }
        current = done.output;
    }
    if ctx.progress.cancelled() {
        return Err(cancelled());
    }
    let output = if current.starts_with(&dir.path) {
        let folder =
            paths::prepare_output_dir(input, folder_setting(&ctx.settings.output, last.name))?;
        publish(&current, &folder)?
    } else {
        current
    };
    Ok(Done {
        output,
        warnings,
        infos,
    })
}

/// Move the finished file out of the work folder into `folder` under a name nothing has yet.
fn publish(result: &Path, folder: &Path) -> Result<PathBuf, RunError> {
    if !fs::symlink_metadata(result)?.is_file() {
        return Err(TaskError::NotAFile.into());
    }
    for _ in 0..16 {
        let target = paths::derive_in(folder, result, "", None);
        match commit_new(result, &target) {
            Ok(()) => return Ok(target),
            // Someone took the name between derive and commit: pick the next one.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "no free output name").into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::folder::{NullProgress, Progress};
    use crate::imaging::{self as im, Format};
    use ::image::{Rgba, RgbaImage};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    fn ctx<'a>(settings: &'a Settings, progress: &'a dyn Progress) -> Ctx<'a> {
        Ctx {
            settings,
            seven_zip: None,
            progress,
            media: None,
            java: None,
            cfr: None,
        }
    }

    fn png(path: &Path, w: u32, h: u32) {
        let img = RgbaImage::from_fn(w, h, |x, y| {
            Rgba([(x % 256) as u8, (y % 256) as u8, 90, 255])
        });
        fs::write(path, im::encode(&img, Format::Png, 0).unwrap()).unwrap();
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
    fn steps_are_checked_strictly() {
        for ok in [
            "image.resize?scale=50",
            "image.resize?scale=12.5",
            "image.resize?scale=custom&w=1080&h=1080&keep=1",
            "image.resize?scale=custom&w=0&h=600",
            "image.rotate?deg=-90",
            "image.compress?bytes=2097152",
            "image.convert?to=JPG",
            "image.strip",
            "video.compress?preset=discord",
            "video.compress?bytes=1048576",
            "video.convert?to=M4A",
            "audio.convert?to=MP3",
            "text.encode?to=SJIS",
            "json.sort",
        ] {
            assert!(parse_step(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "image.resize",
            "image.resize?",
            "image.resize?scale=0",
            "image.resize?scale=1e3",
            "image.resize?scale=nan",
            "image.resize?scale=50&w=10",
            "image.resize?scale=custom&w=0&h=0",
            "image.resize?scale=custom&w=40000&h=1",
            "image.resize?scale=custom&w=10&h=10&keep=2",
            "image.resize?scale=50&scale=60",
            "image.resize?scale=50&x=1",
            "image.resize?scale=%35%30",
            "image.rotate?deg=720",
            "image.compress?bytes=0",
            "image.compress?bytes=-5",
            "image.convert?to=jpg",
            "image.convert?to=HEIC",
            "image.strip?x=1",
            "video.compress?bytes=1&preset=discord",
            "video.compress?preset=other",
            "text.lineEndings?to=CR",
            "json.pretty?",
        ] {
            assert!(
                matches!(parse_step(bad), Err(PlanError::BadStep(_))),
                "{bad}"
            );
        }
        for not_allowed in [
            "folder.removeDuplicate?mode=all",
            "archive.extract",
            "image.ocr",
            "video.trim?start=0&end=1",
            "image.resize.p1",
            "../x",
        ] {
            assert!(
                matches!(parse_step(not_allowed), Err(PlanError::NotAllowed(_))),
                "{not_allowed}"
            );
        }
        assert!(parse_step(&"a".repeat(MAX_STEP_BYTES + 1)).is_err());
    }

    #[test]
    fn chains_are_bounded_and_types_must_connect() {
        assert!(plan(&[
            "image.resize?scale=50",
            "image.convert?to=JPG",
            "image.strip"
        ])
        .is_ok());
        assert!(plan(&["video.convert?to=M4A", "audio.convert?to=MP3"]).is_ok());
        assert!(plan(&["video.convert?to=GIF", "image.resize?scale=50"]).is_ok());
        assert!(plan(&["video.extractAudio?to=WAV", "audio.convert?to=MP3"]).is_ok());
        assert_eq!(plan::<&str>(&[]), Err(PlanError::StepCount));
        assert_eq!(
            plan(&["image.strip"; MAX_STEPS + 1]),
            Err(PlanError::StepCount)
        );
        assert!(plan(&["image.strip"; MAX_STEPS]).is_ok());
        assert_eq!(
            plan(&["image.strip", "video.resize?scale=50"]),
            Err(PlanError::Incompatible(
                "image.strip".into(),
                "video.resize?scale=50".into()
            ))
        );
        assert!(matches!(
            plan(&["video.extractAudio?to=MP3", "video.resize?scale=50"]),
            Err(PlanError::Incompatible(..))
        ));
        assert!(matches!(
            plan(&["text.encode?to=UTF8", "image.strip"]),
            Err(PlanError::Incompatible(..))
        ));
        let e = plan(&["image.strip", "folder.tree"]).unwrap_err();
        assert_eq!(
            (e.key(), e.args()),
            (
                "msg.pipelineStepNotAllowed",
                vec!["folder.tree".to_string()]
            )
        );
    }

    #[test]
    fn every_chainable_task_is_a_file_task_the_engine_knows() {
        for id in chainable_tasks() {
            assert!(tasks::is_file_task(id), "{id}");
        }
    }

    #[test]
    fn resize_convert_strip_writes_only_the_final_file() {
        let (d, tmp) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let src = d.path().join("photo.png");
        png(&src, 200, 100);
        let original = fs::read(&src).unwrap();
        let steps = plan(&[
            "image.resize?scale=50",
            "image.convert?to=JPG",
            "image.strip",
        ])
        .unwrap();
        let s = Settings::default();
        let work = WorkDir::create(tmp.path()).unwrap();
        let mode = fs::metadata(work.root()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        let done = run_file(&steps, &src, &ctx(&s, &NullProgress), &work, 0).unwrap();
        assert_eq!(done.output, d.path().join("photo_resized_clean.jpg"));
        assert_eq!(
            im::load(&done.output).unwrap().still().dimensions(),
            (100, 50)
        );
        assert_eq!(names(d.path()), ["photo.png", "photo_resized_clean.jpg"]);
        assert_eq!(fs::read(&src).unwrap(), original, "original untouched");
        // the per-file folder is gone already; the work folder goes with the guard
        assert_eq!(names(work.root()).len(), 0);
        let root = work.root().to_path_buf();
        drop(work);
        assert!(!root.exists());
        assert_eq!(names(tmp.path()).len(), 0);

        // a second run never overwrites the first result
        let work = WorkDir::create(tmp.path()).unwrap();
        let again = run_file(&steps, &src, &ctx(&s, &NullProgress), &work, 0).unwrap();
        assert_eq!(again.output, d.path().join("photo_resized_clean (2).jpg"));
    }

    #[test]
    fn output_folder_setting_of_the_last_task_is_honoured() {
        let (d, tmp) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let src = d.path().join("notes.txt");
        fs::write(&src, "a\r\nb\r\n").unwrap();
        let mut s = Settings::default();
        s.output.text = "./Converted".into();
        let steps = plan(&["text.lineEndings?to=LF", "text.encode?to=UTF16"]).unwrap();
        let work = WorkDir::create(tmp.path()).unwrap();
        let done = run_file(&steps, &src, &ctx(&s, &NullProgress), &work, 3).unwrap();
        assert_eq!(done.output, d.path().join("Converted/notes_lf_utf16.txt"));
        assert_eq!(names(d.path()), ["Converted", "notes.txt"]);
        assert_eq!(names(&d.path().join("Converted")), ["notes_lf_utf16.txt"]);
    }

    #[test]
    fn a_failing_step_leaves_nothing_anywhere() {
        let (d, tmp) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let src = d.path().join("data.txt");
        fs::write(&src, "not json").unwrap();
        let steps = plan(&["text.lineEndings?to=LF", "json.pretty"]).unwrap();
        let work = WorkDir::create(tmp.path()).unwrap();
        let s = Settings::default();
        let err = run_file(&steps, &src, &ctx(&s, &NullProgress), &work, 0).unwrap_err();
        assert_eq!(err.key(), "msg.invalidJson");
        assert_eq!(names(d.path()), ["data.txt"]);
        assert_eq!(names(work.root()).len(), 0);
    }

    #[test]
    fn wrong_input_kind_is_refused_before_anything_runs() {
        let (d, tmp) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let src = d.path().join("a.txt");
        fs::write(&src, "x").unwrap();
        let steps = plan(&["image.strip"]).unwrap();
        let work = WorkDir::create(tmp.path()).unwrap();
        let s = Settings::default();
        let err = run_file(&steps, &src, &ctx(&s, &NullProgress), &work, 0).unwrap_err();
        assert_eq!(err.key(), "msg.pipelineWrongInput");
        assert!(matches!(
            run_file(&steps, d.path(), &ctx(&s, &NullProgress), &work, 1),
            Err(RunError::WrongInput)
        ));
        assert_eq!(names(d.path()), ["a.txt"]);
    }

    #[test]
    fn a_pass_through_last_step_still_publishes_the_intermediate() {
        let (d, tmp) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let src = d.path().join("p.png");
        png(&src, 64, 64);
        // the resized file is far below 1 GiB, so compress leaves it as it is
        let steps = plan(&["image.resize?scale=50", "image.compress?bytes=1073741824"]).unwrap();
        let s = Settings::default();
        let work = WorkDir::create(tmp.path()).unwrap();
        let done = run_file(&steps, &src, &ctx(&s, &NullProgress), &work, 0).unwrap();
        assert_eq!(done.output, d.path().join("p_resized.png"));
        assert_eq!(done.infos[0].0, "msg.alreadySmall");
        // every step passing the original through creates nothing
        let steps = plan(&["image.compress?bytes=1073741824"]).unwrap();
        let done = run_file(&steps, &src, &ctx(&s, &NullProgress), &work, 1).unwrap();
        assert_eq!(done.output, src);
        assert_eq!(names(d.path()), ["p.png", "p_resized.png"]);
    }

    /// Cancels as soon as the second step starts.
    struct CancelAt {
        stages: AtomicUsize,
        flag: AtomicBool,
    }
    impl Progress for CancelAt {
        fn cancelled(&self) -> bool {
            self.flag.load(Ordering::SeqCst)
        }
        fn status(&self, key: &str, _args: &[String]) {
            if key == "msg.pipelineStage" && self.stages.fetch_add(1, Ordering::SeqCst) == 1 {
                self.flag.store(true, Ordering::SeqCst);
            }
        }
    }

    #[test]
    fn cancel_stops_the_chain_and_cleans_up() {
        let (d, tmp) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let src = d.path().join("c.png");
        png(&src, 300, 300);
        let steps = plan(&[
            "image.resize?scale=50",
            "image.rotate?deg=90",
            "image.strip",
        ])
        .unwrap();
        let s = Settings::default();
        let progress = CancelAt {
            stages: AtomicUsize::new(0),
            flag: AtomicBool::new(false),
        };
        let work = WorkDir::create(tmp.path()).unwrap();
        assert!(run_file(&steps, &src, &ctx(&s, &progress), &work, 0).is_err());
        assert_eq!(
            progress.stages.load(Ordering::SeqCst),
            2,
            "stopped at step 2"
        );
        assert_eq!(names(d.path()), ["c.png"]);
        assert_eq!(names(work.root()).len(), 0);
    }

    #[test]
    fn work_folder_is_exclusive_and_old_ones_are_swept() {
        let tmp = tempfile::tempdir().unwrap();
        // a stale folder of ours is removed; a fresh one, a file and a symlink with the prefix are not
        let stale = tmp.path().join(format!("{BRAND}{WORK_PREFIX}stale"));
        let fresh = tmp.path().join(format!("{BRAND}{WORK_PREFIX}fresh"));
        let file = tmp.path().join(format!("{BRAND}{WORK_PREFIX}file"));
        let link = tmp.path().join(format!("{BRAND}{WORK_PREFIX}link"));
        let target = tmp.path().join("precious");
        fs::create_dir(&stale).unwrap();
        fs::create_dir(&fresh).unwrap();
        fs::create_dir(&target).unwrap();
        fs::write(target.join("keep.txt"), "keep").unwrap();
        fs::write(&file, "x").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let old = SystemTime::now() - Duration::from_secs(3 * 24 * 3600);
        for p in [&stale, &file] {
            fs::File::open(p).unwrap().set_modified(old).unwrap();
        }
        let work = WorkDir::create(tmp.path()).unwrap();
        assert!(!stale.exists());
        assert!(fresh.exists() && file.exists() && link.symlink_metadata().is_ok());
        assert_eq!(fs::read(target.join("keep.txt")).unwrap(), b"keep");
        assert!(work.root().is_dir());
    }

    #[test]
    fn json_steps_use_the_text_output_folder() {
        let o = OutputSettings {
            text: "t".into(),
            ..Default::default()
        };
        assert_eq!(folder_setting(&o, "json.pretty"), "t");
        assert_eq!(folder_setting(&o, "text.encode"), "t");
        assert_eq!(folder_setting(&o, "image.strip"), "");
    }
}
