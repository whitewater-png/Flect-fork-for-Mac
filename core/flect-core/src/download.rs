//! Media download through the user's own `yt-dlp` (see NOTICES.md: not bundled, not
//! fetched by Flect). Off by default (`Settings.download.enabled`).
//!
//! The command line is built here and run without a shell. Nothing here handles logins,
//! cookies or DRM, and the argument builder takes no free-form options, so a caller cannot
//! smuggle a yt-dlp option in through a URL or a format string.
use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::io::{self, BufRead, BufReader, Read};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::archive::find_tool;
use crate::folder::Progress;
use crate::paths;
use crate::pipeline::WorkDir;
use crate::settings::Settings;

pub const TOOL_NAME: &str = "yt-dlp";
/// What the tool (and the ffmpeg it starts) may search: the fixed install folders and the system's.
pub const PATH_ENV: &str = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin";
const MAX_URL_BYTES: usize = 2048;
/// Longest video height accepted for a quality cap (8K).
const MAX_HEIGHT: u32 = 4320;
/// Quality caps the protocol accepts.
pub const HEIGHTS: [u32; 5] = [480, 720, 1080, 1440, 2160];
/// A single download larger than this is refused by yt-dlp.
const MAX_FILE_SIZE: &str = "8G";
const PROGRESS_PREFIX: &str = "flect-progress ";
const FILE_PREFIX: &str = "flect-file ";
/// No output at all for this long means the transfer is stuck.
const STALL_LIMIT: Duration = Duration::from_secs(300);
const POLL: Duration = Duration::from_millis(40);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlError {
    Empty,
    TooLong,
    /// Whitespace or a control character anywhere in the text.
    BadCharacter,
    /// Not `http://` or `https://`.
    Scheme,
    /// Missing host, or credentials embedded in the address.
    Host,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// MP4, optionally capped to a height (e.g. 720).
    Video { max_height: Option<u32> },
    /// MP3 audio only.
    Audio,
}

/// The request's `kind` / `max_height` as a `Kind`; anything outside the offered choices is refused.
pub fn kind_from(kind: &str, max_height: Option<u32>) -> Option<Kind> {
    match (kind, max_height) {
        ("audio", None) => Some(Kind::Audio),
        ("video", None) => Some(Kind::Video { max_height: None }),
        ("video", Some(h)) if HEIGHTS.contains(&h) => Some(Kind::Video {
            max_height: Some(h),
        }),
        _ => None,
    }
}

/// Accepts a plain web address and returns it trimmed.
pub fn validate_url(raw: &str) -> Result<String, UrlError> {
    let url = raw.trim();
    if url.is_empty() {
        return Err(UrlError::Empty);
    }
    if url.len() > MAX_URL_BYTES {
        return Err(UrlError::TooLong);
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(UrlError::BadCharacter);
    }
    let lower = url.to_ascii_lowercase();
    let rest = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
        .ok_or(UrlError::Scheme)?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') {
        return Err(UrlError::Host);
    }
    Ok(url.to_string())
}

/// The host part of a validated address, for the status line.
pub fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split(['/', '?', '#']).next().unwrap_or("").to_string()
}

/// Arguments for one download into `out_dir`; unfinished parts stay in `temp_dir`. `url` must
/// already have passed `validate_url`. The address comes after `--`, so it is never read as an option.
pub fn build_args(url: &str, kind: Kind, out_dir: &Path, temp_dir: &Path) -> Vec<String> {
    let mut args: Vec<String> = [
        // Ignore the user's own yt-dlp config files so results do not depend on them.
        "--ignore-config",
        "--no-playlist",
        "--newline",
        "--progress",
        "--max-filesize",
        MAX_FILE_SIZE,
        "--progress-template",
    ]
    .map(String::from)
    .to_vec();
    args.push(format!(
        "download:{PROGRESS_PREFIX}%(progress.downloaded_bytes)s %(progress.total_bytes)s %(progress.total_bytes_estimate)s"
    ));
    args.push("--print".into());
    args.push(format!("after_move:{FILE_PREFIX}%(filepath)s"));
    args.push("-P".into());
    args.push(out_dir.to_string_lossy().into_owned());
    args.push("-P".into());
    args.push(format!("temp:{}", temp_dir.to_string_lossy()));
    args.push("-o".into());
    // An audio download gets its own name. With the video's name, yt-dlp would take a video of the
    // same address already in the folder for its source, convert it and delete it.
    args.push(
        match kind {
            Kind::Audio => "%(title).100B [%(id)s] (audio).%(ext)s",
            Kind::Video { .. } => "%(title).100B [%(id)s].%(ext)s",
        }
        .into(),
    );
    match kind {
        Kind::Audio => {
            args.extend(["-x", "--audio-format", "mp3"].map(String::from));
        }
        Kind::Video { max_height } => {
            let selector = match max_height.filter(|h| (1..=MAX_HEIGHT).contains(h)) {
                // `<=?` also accepts a format whose height the site does not report.
                Some(h) => format!("bv*[height<=?{h}]+ba/b[height<=?{h}]"),
                None => "bv*+ba/b".to_string(),
            };
            args.extend([
                "-f".into(),
                selector,
                "--merge-output-format".into(),
                "mp4".into(),
            ]);
        }
    }
    args.push("--".into());
    args.push(url.to_string());
    args
}

/// The installed `yt-dlp`, from the same fixed directories as ffmpeg.
pub fn find() -> Option<PathBuf> {
    find_tool(TOOL_NAME)
}

/// Checks done before anything is run. The error is a message key for the user.
pub fn preflight(
    settings: &Settings,
    tool: Option<PathBuf>,
    has_ffmpeg: bool,
) -> Result<PathBuf, &'static str> {
    if !settings.download.enabled {
        return Err("download.disabled");
    }
    let tool = tool.ok_or("download.toolMissing")?;
    if !has_ffmpeg {
        return Err("download.needFfmpeg");
    }
    Ok(tool)
}

/// Where downloads go: `Output.Download`, or `<home>/Downloads` when empty (a relative setting
/// starts at `home`). Created when missing; a link in the way is refused like for other outputs.
pub fn destination(settings: &Settings, home: &Path) -> io::Result<PathBuf> {
    let setting = settings.output.download.trim();
    let default = home.join("Downloads");
    let setting = if setting.is_empty() {
        default.to_string_lossy()
    } else {
        setting.into()
    };
    paths::prepare_output_dir(&home.join("download"), &setting)
}

/// `flect-progress <downloaded> <total> <estimate>` -> 0..=1000, when the size is known.
pub fn parse_progress(line: &str) -> Option<u32> {
    let mut parts = line.strip_prefix(PROGRESS_PREFIX)?.split_whitespace();
    let number = |s: Option<&str>| s.and_then(|v| v.parse::<f64>().ok());
    let downloaded = number(parts.next())?;
    let total = number(parts.next()).or_else(|| number(parts.next()))?;
    (downloaded >= 0.0 && total > 0.0)
        .then(|| ((downloaded / total).clamp(0.0, 1.0) * 1000.0) as u32)
}

/// `flect-file <path>` -> the finished file's path.
pub fn parse_file(line: &str) -> Option<PathBuf> {
    let p = line.strip_prefix(FILE_PREFIX)?.trim_end_matches('\r');
    (p.starts_with('/') && !p.contains('\0')).then(|| PathBuf::from(p))
}

#[derive(Debug)]
pub enum RunError {
    Cancelled,
    /// Nothing was printed for `STALL_LIMIT`.
    Stalled,
    Io(io::Error),
    Failed {
        code: Option<i32>,
        tail: String,
    },
}

impl From<io::Error> for RunError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// What the tool is given: no inherited environment at all.
#[derive(Clone, Copy)]
pub struct Env<'a> {
    pub path: &'a str,
    pub home: Option<&'a Path>,
}

fn read_capped(r: impl Read, cap: u64) -> String {
    let mut buf = Vec::new();
    let _ = r.take(cap).read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

/// Stops the tool and everything it started (it runs ffmpeg as a child), before it is reaped.
fn kill_group(child: &mut Child) {
    if let Some(pid) = i32::try_from(child.id())
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    {
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn absorb(line: &str, progress: &dyn Progress, files: &mut Vec<PathBuf>) {
    if let Some(permille) = parse_progress(line) {
        progress.file_step(permille);
    } else if let Some(path) = parse_file(line) {
        if !files.contains(&path) {
            files.push(path);
        }
    }
}

/// Runs the tool to completion and returns the finished files it printed. Cancelling (or a stall)
/// kills its whole process group.
pub fn run(
    tool: &Path,
    args: &[String],
    env: Env<'_>,
    progress: &dyn Progress,
) -> Result<Vec<PathBuf>, RunError> {
    let mut command = Command::new(tool);
    command
        .args(args)
        .env_clear()
        .env("PATH", env.path)
        .env("PYTHONUTF8", "1");
    if let Some(home) = env.home {
        command.env("HOME", home);
    }
    let mut child = command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    let err_thread = std::thread::spawn(move || read_capped(stderr, 64 * 1024));
    let (tx, rx) = mpsc::channel::<String>();
    // Not joined on the kill paths: a helper that kept the pipe open must not hold us up.
    let out_thread = std::thread::spawn(move || {
        for line in BufReader::new(stdout.take(64 << 20))
            .lines()
            .map_while(Result::ok)
        {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut files = Vec::new();
    let mut last_output = Instant::now();
    let status = loop {
        if progress.cancelled() {
            kill_group(&mut child);
            return Err(RunError::Cancelled);
        }
        while let Ok(line) = rx.try_recv() {
            last_output = Instant::now();
            absorb(&line, progress, &mut files);
        }
        if last_output.elapsed() > STALL_LIMIT {
            kill_group(&mut child);
            return Err(RunError::Stalled);
        }
        match child.try_wait()? {
            Some(s) => break s,
            None => std::thread::sleep(POLL),
        }
    };
    // What was printed just before the exit; a helper that keeps the pipe open is not waited for.
    while let Ok(line) = rx.recv_timeout(Duration::from_millis(500)) {
        absorb(&line, progress, &mut files);
    }
    drop(out_thread);
    let err = err_thread.join().unwrap_or_default();
    if status.success() {
        Ok(files)
    } else {
        use std::os::unix::process::ExitStatusExt;
        let tail = err
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .rev()
            .take(3)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join(" ");
        Err(RunError::Failed {
            code: status.code().or(status.signal().map(|s| -s)),
            tail: tail.chars().take(300).collect(),
        })
    }
}

pub struct Job<'a> {
    pub tool: &'a Path,
    pub url: &'a str,
    pub kind: Kind,
    pub out_dir: &'a Path,
    pub env: Env<'a>,
    /// Where the private work folder for unfinished parts is made (removed afterwards).
    pub work_parent: &'a Path,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Fetched {
    /// Files this download made.
    pub created: Vec<PathBuf>,
    /// Files it reported that were already in the folder: left alone, never reported as new, so
    /// "Undo" cannot touch them.
    pub existing: Vec<PathBuf>,
}

fn names_in(dir: &Path) -> HashSet<OsString> {
    fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.file_name()).collect())
        .unwrap_or_default()
}

/// One download. Only regular files directly inside `out_dir` are accepted from what the tool printed.
pub fn download(job: &Job<'_>, progress: &dyn Progress) -> Result<Fetched, RunError> {
    let before = names_in(job.out_dir);
    let work = WorkDir::create(job.work_parent)?;
    let args = build_args(job.url, job.kind, job.out_dir, work.root());
    let reported = run(job.tool, &args, job.env, progress)?;
    let mut fetched = Fetched::default();
    for path in reported {
        let in_folder = path.parent() == Some(job.out_dir);
        let regular = fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file());
        let Some(name) = path.file_name().map(OsString::from) else {
            continue;
        };
        if !(in_folder && regular) {
            continue;
        }
        if before.contains(&name) {
            fetched.existing.push(path);
        } else {
            fetched.created.push(path);
        }
    }
    Ok(fetched)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn accepts_plain_web_addresses() {
        assert_eq!(
            validate_url("  https://example.com/watch?v=abc123  ").unwrap(),
            "https://example.com/watch?v=abc123"
        );
        assert!(validate_url("HTTP://example.com").is_ok());
    }

    #[test]
    fn rejects_everything_else() {
        assert_eq!(validate_url("   "), Err(UrlError::Empty));
        assert_eq!(validate_url("--exec=rm"), Err(UrlError::Scheme));
        assert_eq!(validate_url("file:///etc/passwd"), Err(UrlError::Scheme));
        assert_eq!(validate_url("ftp://example.com/a"), Err(UrlError::Scheme));
        assert_eq!(validate_url("https://"), Err(UrlError::Host));
        assert_eq!(validate_url("https:///path"), Err(UrlError::Host));
        assert_eq!(
            validate_url("https://user:pass@example.com/"),
            Err(UrlError::Host)
        );
        assert_eq!(
            validate_url("https://example.com/a b"),
            Err(UrlError::BadCharacter)
        );
        assert_eq!(
            validate_url("https://example.com/\n--exec=x"),
            Err(UrlError::BadCharacter)
        );
        let long = format!("https://example.com/{}", "a".repeat(MAX_URL_BYTES));
        assert_eq!(validate_url(&long), Err(UrlError::TooLong));
    }

    #[test]
    fn only_the_offered_choices_become_a_kind() {
        assert_eq!(kind_from("audio", None), Some(Kind::Audio));
        assert_eq!(
            kind_from("video", None),
            Some(Kind::Video { max_height: None })
        );
        assert_eq!(
            kind_from("video", Some(720)),
            Some(Kind::Video {
                max_height: Some(720)
            })
        );
        for bad in [
            kind_from("video", Some(721)),
            kind_from("video", Some(0)),
            kind_from("audio", Some(720)),
            kind_from("playlist", None),
            kind_from("", None),
        ] {
            assert_eq!(bad, None);
        }
    }

    #[test]
    fn host_for_the_status_line() {
        assert_eq!(host_of("https://example.com/a?b=c"), "example.com");
        assert_eq!(host_of("http://a.example:8080#x"), "a.example:8080");
    }

    fn tail(a: &[String], n: usize) -> &[String] {
        &a[a.len() - n..]
    }

    #[test]
    fn video_arguments() {
        let a = build_args(
            "https://example.com/v",
            Kind::Video {
                max_height: Some(720),
            },
            Path::new("/tmp/out"),
            Path::new("/tmp/work"),
        );
        assert_eq!(
            a,
            [
                "--ignore-config",
                "--no-playlist",
                "--newline",
                "--progress",
                "--max-filesize",
                "8G",
                "--progress-template",
                "download:flect-progress %(progress.downloaded_bytes)s %(progress.total_bytes)s %(progress.total_bytes_estimate)s",
                "--print",
                "after_move:flect-file %(filepath)s",
                "-P",
                "/tmp/out",
                "-P",
                "temp:/tmp/work",
                "-o",
                "%(title).100B [%(id)s].%(ext)s",
                "-f",
                "bv*[height<=?720]+ba/b[height<=?720]",
                "--merge-output-format",
                "mp4",
                "--",
                "https://example.com/v",
            ]
        );
    }

    #[test]
    fn unbounded_or_absurd_heights_fall_back_to_best() {
        for h in [None, Some(0), Some(MAX_HEIGHT + 1)] {
            let a = build_args(
                "https://example.com/v",
                Kind::Video { max_height: h },
                Path::new("/o"),
                Path::new("/w"),
            );
            assert!(a.contains(&"bv*+ba/b".to_string()), "{h:?}");
        }
    }

    #[test]
    fn audio_arguments() {
        let a = build_args(
            "https://example.com/v",
            Kind::Audio,
            Path::new("/o"),
            Path::new("/w"),
        );
        assert!(a.windows(3).any(|w| w == ["-x", "--audio-format", "mp3"]));
        assert!(!a.iter().any(|s| s == "-f"));
        // never the same name as the video of that address
        assert!(a.contains(&"%(title).100B [%(id)s] (audio).%(ext)s".to_string()));
        assert!(!a.contains(&"%(title).100B [%(id)s].%(ext)s".to_string()));
    }

    #[test]
    fn the_address_always_follows_the_option_terminator() {
        for kind in [Kind::Audio, Kind::Video { max_height: None }] {
            let a = build_args(
                "https://example.com/v",
                kind,
                Path::new("/o"),
                Path::new("/w"),
            );
            assert_eq!(tail(&a, 2), ["--", "https://example.com/v"]);
        }
    }

    #[test]
    fn progress_and_file_lines() {
        assert_eq!(parse_progress("flect-progress 250 1000 NA"), Some(250));
        assert_eq!(parse_progress("flect-progress 250 NA 500"), Some(500));
        assert_eq!(parse_progress("flect-progress 2000 1000 NA"), Some(1000));
        assert_eq!(parse_progress("flect-progress 10 NA NA"), None);
        assert_eq!(parse_progress("flect-progress 10 0 NA"), None);
        assert_eq!(parse_progress("flect-progress x 1 1"), None);
        assert_eq!(parse_progress("[download] 10%"), None);
        assert_eq!(
            parse_file("flect-file /Users/a/My Video [id].mp4"),
            Some(PathBuf::from("/Users/a/My Video [id].mp4"))
        );
        assert_eq!(parse_file("flect-file relative.mp4"), None);
        assert_eq!(parse_file("flect-file "), None);
        assert_eq!(parse_file("/Users/a/b.mp4"), None);
    }

    #[test]
    fn preflight_order() {
        let mut s = Settings::default();
        assert_eq!(
            preflight(&s, Some("/t".into()), true),
            Err("download.disabled")
        );
        s.download.enabled = true;
        assert_eq!(preflight(&s, None, true), Err("download.toolMissing"));
        assert_eq!(
            preflight(&s, Some("/t".into()), false),
            Err("download.needFfmpeg")
        );
        assert_eq!(
            preflight(&s, Some("/t".into()), true),
            Ok(PathBuf::from("/t"))
        );
    }

    #[test]
    fn destination_defaults_to_downloads_and_follows_the_setting() {
        let home = tempfile::tempdir().unwrap();
        let mut s = Settings::default();
        let d = destination(&s, home.path()).unwrap();
        assert_eq!(d, home.path().join("Downloads"));
        assert!(d.is_dir());
        s.output.download = "Saved/videos".into();
        assert_eq!(
            destination(&s, home.path()).unwrap(),
            home.path().join("Saved/videos")
        );
        let elsewhere = tempfile::tempdir().unwrap();
        s.output.download = elsewhere.path().join("x").to_string_lossy().into_owned();
        assert_eq!(
            destination(&s, home.path()).unwrap(),
            elsewhere.path().join("x")
        );
    }

    #[test]
    fn a_link_as_the_default_folder_is_refused() {
        let home = tempfile::tempdir().unwrap();
        let real = home.path().join("real");
        fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, home.path().join("Downloads")).unwrap();
        let e = destination(&Settings::default(), home.path()).unwrap_err();
        assert_eq!(paths::io_error_key(&e), "msg.outputFolderLink");
    }

    struct Flag(AtomicBool);
    impl Progress for Flag {
        fn cancelled(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }
    }

    struct Seen(std::sync::Mutex<Vec<u32>>);
    impl Progress for Seen {
        fn file_step(&self, permille: u32) {
            self.0.lock().unwrap().push(permille);
        }
    }

    /// A stand-in for yt-dlp: a shell script that follows `-P`, prints the progress and file lines.
    fn fake_tool(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("yt-dlp");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    const OUT_DIR_FROM_ARGS: &str = r#"
out=""
while [ $# -gt 0 ]; do
  case "$1" in
    -P) shift; case "$1" in temp:*) ;; *) out="$1";; esac;;
  esac
  shift
done
"#;

    fn env() -> Env<'static> {
        Env {
            path: "/usr/bin:/bin",
            home: None,
        }
    }

    #[test]
    fn a_download_reports_progress_and_only_new_files() {
        let tools = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        fs::write(out.path().join("old.mp4"), "old").unwrap();
        let body = format!(
            r#"{OUT_DIR_FROM_ARGS}
echo "flect-progress 100 400 NA"
echo "flect-progress 400 400 NA"
echo new > "$out/new.mp4"
echo "flect-file $out/new.mp4"
echo "flect-file $out/old.mp4"
echo "flect-file /etc/passwd"
echo "flect-file $out/missing.mp4"
"#
        );
        let tool = fake_tool(tools.path(), &body);
        let job = Job {
            tool: &tool,
            url: "https://example.com/v",
            kind: Kind::Audio,
            out_dir: out.path(),
            env: env(),
            work_parent: tools.path(),
        };
        let seen = Seen(Default::default());
        let r = download(&job, &seen).unwrap();
        assert_eq!(r.created, [out.path().join("new.mp4")]);
        assert_eq!(r.existing, [out.path().join("old.mp4")]);
        assert_eq!(*seen.0.lock().unwrap(), [250, 1000]);
        // the work folder is gone
        let left: Vec<_> = names_in(tools.path())
            .into_iter()
            .filter(|n| n.to_string_lossy().contains("pipeline"))
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }

    #[test]
    fn the_tool_gets_no_inherited_environment() {
        let tools = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        std::env::set_var("FLECT_TEST_SECRET", "leak");
        let body = format!(
            r#"{OUT_DIR_FROM_ARGS}
echo "${{FLECT_TEST_SECRET:-clean}}" > "$out/env.txt"
echo "$PATH" >> "$out/env.txt"
echo "flect-file $out/env.txt"
"#
        );
        let tool = fake_tool(tools.path(), &body);
        let job = Job {
            tool: &tool,
            url: "https://example.com/v",
            kind: Kind::Audio,
            out_dir: out.path(),
            env: env(),
            work_parent: tools.path(),
        };
        let r = download(&job, &crate::folder::NullProgress).unwrap();
        assert_eq!(r.created.len(), 1);
        let text = fs::read_to_string(out.path().join("env.txt")).unwrap();
        assert_eq!(text, "clean\n/usr/bin:/bin\n");
    }

    #[test]
    fn a_failing_tool_reports_the_end_of_its_error_output() {
        let tools = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let tool = fake_tool(
            tools.path(),
            "echo 'WARNING: first' >&2\necho 'ERROR: Unsupported URL' >&2\nexit 1",
        );
        let job = Job {
            tool: &tool,
            url: "https://example.com/v",
            kind: Kind::Audio,
            out_dir: out.path(),
            env: env(),
            work_parent: tools.path(),
        };
        match download(&job, &crate::folder::NullProgress) {
            Err(RunError::Failed { code, tail }) => {
                assert_eq!(code, Some(1));
                assert_eq!(tail, "WARNING: first ERROR: Unsupported URL");
            }
            other => panic!("{other:?}"),
        }
    }

    /// Against a real yt-dlp and a local file server; run by hand:
    /// `FLECT_TEST_YTDLP=/path/to/yt-dlp FLECT_TEST_YTDLP_URL=http://127.0.0.1:8765/clip.mp4
    ///  cargo test -p flect-core real_ytdlp -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_ytdlp_downloads_a_local_file() {
        let (Some(tool), Some(url)) = (
            std::env::var_os("FLECT_TEST_YTDLP").map(PathBuf::from),
            std::env::var("FLECT_TEST_YTDLP_URL").ok(),
        ) else {
            return;
        };
        let scratch = tempfile::tempdir().unwrap();
        let out = scratch.path().join("out");
        fs::create_dir(&out).unwrap();
        for kind in [
            Kind::Video { max_height: None },
            Kind::Video {
                max_height: Some(480),
            },
            Kind::Audio,
        ] {
            let seen = Seen(Default::default());
            let job = Job {
                tool: &tool,
                url: &url,
                kind,
                out_dir: &out,
                env: Env {
                    path: PATH_ENV,
                    home: Some(scratch.path()),
                },
                work_parent: scratch.path(),
            };
            let r = download(&job, &seen).unwrap_or_else(|e| panic!("{kind:?}: {e:?}"));
            println!("{kind:?}: {r:?}, progress {:?}", seen.0.lock().unwrap());
            assert_eq!(r.created.len() + r.existing.len(), 1, "{kind:?}");
            for p in r.created.iter().chain(&r.existing) {
                assert!(fs::metadata(p).unwrap().len() > 0, "{p:?}");
            }
        }
        // exactly the two finished files: the audio run did not touch the video
        let mut names: Vec<_> = names_in(&out)
            .into_iter()
            .map(|n| n.to_string_lossy().into_owned())
            .collect();
        names.sort();
        println!("{names:?}");
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(names.iter().any(|n| n.ends_with("[clip].mp4")), "{names:?}");
        assert!(
            names.iter().any(|n| n.ends_with("[clip] (audio).mp3")),
            "{names:?}"
        );
    }

    #[test]
    fn cancelling_stops_the_tool_and_what_it_started() {
        let tools = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let marker = tools.path().join("child.pid");
        // the "tool" starts a child that would outlive it
        let body = format!(
            "sleep 30 &\necho $! > '{}'\necho 'flect-progress 1 100 NA'\nwait",
            marker.display()
        );
        let tool = fake_tool(tools.path(), &body);
        let flag = Flag(AtomicBool::new(false));
        let started = Instant::now();
        std::thread::scope(|s| {
            s.spawn(|| {
                while !marker.exists() {
                    std::thread::sleep(Duration::from_millis(20));
                }
                std::thread::sleep(Duration::from_millis(100));
                flag.0.store(true, Ordering::SeqCst);
            });
            let job = Job {
                tool: &tool,
                url: "https://example.com/v",
                kind: Kind::Audio,
                out_dir: out.path(),
                env: env(),
                work_parent: tools.path(),
            };
            assert!(matches!(download(&job, &flag), Err(RunError::Cancelled)));
        });
        assert!(started.elapsed() < Duration::from_secs(10));
        let pid: i32 = fs::read_to_string(&marker).unwrap().trim().parse().unwrap();
        // the child is gone (or a zombie awaiting its parent, which no longer exists)
        let alive = fs::read_to_string(format!("/proc/{pid}/stat"))
            .map(|s| !s.contains(") Z "))
            .unwrap_or(false);
        assert!(!alive, "the child process survived the cancel");
    }
}
