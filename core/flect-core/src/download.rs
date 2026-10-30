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
/// The largest playlist a single download may expand to (`--playlist-items 1..N`).
pub const MAX_PLAYLIST_ITEMS: u32 = 20;
/// The system sips: it decodes HEIC / HEIF / AVIF, which the pipeline's own decoders do not.
pub const SIPS: &str = "/usr/bin/sips";
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
    /// An image fetched with the system `curl` (no yt-dlp, no ffmpeg). Optional pipeline steps
    /// (`image.resize?scale=50`, ...) can be applied to the fetched file in the same run.
    Image,
}

/// The request's `kind` / `max_height` as a `Kind`; anything outside the offered choices is refused.
pub fn kind_from(kind: &str, max_height: Option<u32>) -> Option<Kind> {
    match (kind, max_height) {
        ("audio", None) => Some(Kind::Audio),
        ("video", None) => Some(Kind::Video { max_height: None }),
        ("video", Some(h)) if HEIGHTS.contains(&h) => Some(Kind::Video {
            max_height: Some(h),
        }),
        ("image", None) => Some(Kind::Image),
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

/// Whether the address points at this machine or a private network: loopback, RFC 1918,
/// link-local (which includes the cloud metadata address), CGNAT, IPv6 loopback / unique-local /
/// link-local, and `.localhost` / `.local` names. DNS names are deliberately not resolved —
/// the check is on what the user typed, which catches the accidental and metadata cases without
/// putting a lookup in front of every download.
pub fn is_local_address(url: &str) -> bool {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h).to_ascii_lowercase();
    if let Some(rest) = host.strip_prefix('[') {
        let inner = rest.split(']').next().unwrap_or("");
        return inner.parse::<std::net::Ipv6Addr>().is_ok_and(|v6| is_local_v6(&v6));
    }
    // Names and IPv4 literals both lose a `:port` before the checks.
    let name = host.split(':').next().unwrap_or("");
    if name == "localhost" || name.ends_with(".localhost") || name.ends_with(".local") {
        return true;
    }
    match name.parse::<std::net::Ipv4Addr>() {
        Ok(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || (o[0] == 100 && (64..=127).contains(&o[1])) // CGNAT (100.64.0.0/10)
        }
        Err(_) => false,
    }
}

fn is_local_v6(v6: &std::net::Ipv6Addr) -> bool {
    let head = v6.segments()[0];
    v6.is_loopback()
        || *v6 == std::net::Ipv6Addr::UNSPECIFIED
        || (head & 0xfe00) == 0xfc00 // fc00::/7 unique-local
        || (head & 0xffc0) == 0xfe80 // fe80::/10 link-local
}

/// Arguments for one download into `out_dir`; unfinished parts stay in `temp_dir`. `url` must
/// already have passed `validate_url`. The address comes after `--`, so it is never read as an option.
pub fn build_args(
    url: &str,
    kind: Kind,
    out_dir: &Path,
    temp_dir: &Path,
    playlist_items: u32,
) -> Vec<String> {
    // `--no-playlist` only helps for an address of one video inside a list. An address that is a
    // list itself would otherwise be saved item by item, hundreds of files for a big one — so the
    // caller says how many items of a list are wanted (`1..N`, capped by MAX_PLAYLIST_ITEMS).
    let items = if playlist_items <= 1 {
        "1".to_string()
    } else {
        format!("1..{playlist_items}")
    };
    let mut args: Vec<String> = [
        // Ignore the user's own yt-dlp config files so results do not depend on them.
        "--ignore-config",
        "--no-playlist",
        "--playlist-items",
        items.as_str(),
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
            // Images are fetched with the system curl (`fetch_image`), never through yt-dlp.
            Kind::Image => unreachable!("build_args runs only for video and audio"),
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
        Kind::Image => unreachable!("build_args runs only for video and audio"),
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
    /// How many items of an address that is a playlist itself are saved (`--playlist-items 1..N`).
    /// 1 keeps the one-file behaviour; ignored for images (they never go through yt-dlp).
    pub playlist_items: u32,
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
    let args = build_args(job.url, job.kind, job.out_dir, work.root(), job.playlist_items);
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

/// The largest image a download may save.
pub const IMAGE_MAX_BYTES: u64 = 50 * 1024 * 1024;
const IMAGE_MAX_TIME: &str = "120";
const IMAGE_HEAD_MAX_TIME: &str = "30";
/// The system curl. Images are fetched with it (no yt-dlp): fixed arguments only, the address
/// comes after `--`, so nothing from the request can become a curl option.
pub const CURL: &str = "/usr/bin/curl";

/// The system curl, when it is there (it is on every macOS).
pub fn find_curl() -> Option<PathBuf> {
    Path::new(CURL).exists().then(|| PathBuf::from(CURL))
}

/// Checks done before an image download. yt-dlp and ffmpeg are not needed for these.
pub fn preflight_image(settings: &Settings, curl: Option<PathBuf>) -> Result<PathBuf, &'static str> {
    if !settings.download.enabled {
        return Err("download.disabled");
    }
    curl.ok_or("download.toolMissing")
}

pub struct ImageJob<'a> {
    pub tool: &'a Path,
    pub url: &'a str,
    pub out_dir: &'a Path,
    pub env: Env<'a>,
    /// Where the private work folder for the unfinished download is made (removed afterwards).
    pub work_parent: &'a Path,
}

#[derive(Debug)]
pub enum ImageError {
    Cancelled,
    Io(io::Error),
    /// curl failed or timed out. The tail of its stderr.
    Failed(String),
    /// The reply says more than `IMAGE_MAX_BYTES`, or the file on disk is bigger than it.
    TooLarge,
    /// Not an image: a non-200 reply, a non-image content type, or bytes that are not a known
    /// image header (the content type alone is never trusted).
    NotImage,
}

impl From<io::Error> for ImageError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// The extension a content type maps to, when it is one of the accepted image types.
fn image_extension(ctype_base: &str) -> Option<&'static str> {
    Some(match ctype_base {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/heic" => "heic",
        "image/heif" => "heif",
        "image/avif" => "avif",
        "image/bmp" | "image/x-ms-bmp" => "bmp",
        "image/tiff" => "tif",
        "image/svg+xml" => "svg",
        _ => return None,
    })
}

/// Whether the first bytes really are this image type. The content type only picks the extension;
/// this decides. (An `image/png` that is actually a script is refused here.)
fn image_header_matches(head: &[u8], ext: &str) -> bool {
    let starts = |sig: &[u8]| head.starts_with(sig);
    match ext {
        "png" => starts(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]),
        "jpg" => starts(&[0xff, 0xd8, 0xff]),
        "gif" => starts(b"GIF8"),
        "webp" => starts(b"RIFF") && head.get(8..12) == Some(b"WEBP"),
        "bmp" => starts(b"BM"),
        "tif" => starts(b"II*\0") || starts(b"MM\0*"),
        "svg" => starts(b"<"),
        "heic" | "heif" | "avif" => {
            head.get(4..8) == Some(b"ftyp")
                && matches!(
                    head.get(8..12),
                    Some(b"heic") | Some(b"heix") | Some(b"mif1") | Some(b"heim")
                        | Some(b"heis") | Some(b"avif") | Some(b"avis")
                )
        }
        _ => false,
    }
}

/// A file name for the fetched image: the last path segment of the address, sanitized
/// (ASCII letters, digits, `.`, `-`, `_`; no leading dot; at most 60 characters), or the host
/// when the path gives nothing usable. The extension comes from the content type.
fn image_stem(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or("");
    let last = path.rsplit('/').next().unwrap_or("");
    let cleaned: String = last
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '-' || *c == '_')
        .collect();
    let limited: String = cleaned.chars().take(60).collect();
    // Drop a trailing extension (the reply's own extension is added later, so `photo.png`
    // becomes `photo.png` once, not `photo.png.png`). A dotfile like `.hidden` keeps its name;
    // a lone extension such as `.png` is not a name, and the host is used instead.
    let stem = match limited.rfind('.') {
        Some(i) if limited.len() - i <= 6 && limited[i + 1..].chars().all(|c| c.is_ascii_alphanumeric()) => {
            limited[..i].trim_matches('.').to_string()
        }
        _ => limited.trim_start_matches('.').trim_matches('.').to_string(),
    };
    if stem.is_empty() {
        host_of(url)
    } else {
        stem
    }
}

/// A name in `dir` that does not exist yet: `stem.ext`, then `stem (2).ext`, `stem (3).ext`, ...
/// (the same never-overwrite rule as every other Flect output).
pub fn free_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let name = |n: u32| {
        if ext.is_empty() {
            if n == 1 { stem.to_string() } else { format!("{stem} ({n})") }
        } else if n == 1 {
            format!("{stem}.{ext}")
        } else {
            format!("{stem} ({n}).{ext}")
        }
    };
    let mut n = 1u32;
    loop {
        let candidate = dir.join(name(n));
        if fs::symlink_metadata(&candidate).is_err() {
            return candidate;
        }
        n += 1;
    }
}

/// Convert an image the pipeline cannot decode (HEIC / HEIF / AVIF) to PNG with the system sips,
/// so pipeline steps can run on the result. sips ships with macOS; a failure is the caller's to
/// report — the original file stays untouched.
pub fn convert_to_png(src: &Path, dst: &Path) -> Result<(), ImageError> {
    let out = Command::new(SIPS)
        .args(["-s", "format", "png", &src.to_string_lossy(), "--out", &dst.to_string_lossy()])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()?;
    if out.status.success() && fs::symlink_metadata(dst).is_ok_and(|m| m.file_type().is_file()) {
        Ok(())
    } else {
        Err(ImageError::Failed(
            String::from_utf8_lossy(&out.stderr).chars().take(200).collect(),
        ))
    }
}

/// curl's reply line (`%{http_code} %{content_type} %{size_download}`) split.
fn parse_curl_report(report: &str) -> Option<(String, String, u64)> {
    let mut parts = report.split(' ');
    let code = parts.next()?.to_string();
    let ctype = parts.next().unwrap_or("").to_string();
    let size = parts.next()?.parse::<u64>().ok()?;
    Some((code, ctype, size))
}

/// Runs curl with nothing inherited. `url` must already have passed `validate_url`.
fn run_curl(tool: &Path, env: &Env<'_>, args: &[&str]) -> Result<std::process::Output, ImageError> {
    let mut command = Command::new(tool);
    command
        .args(args)
        .env_clear()
        .env("PATH", env.path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(home) = env.home {
        command.env("HOME", home);
    }
    Ok(command.output()?)
}

/// curl for the GET, watched while it runs: the part file's growth is reported as file progress
/// (when the HEAD said a length) and cancelling kills curl mid-transfer instead of only after.
fn run_curl_progress(
    tool: &Path,
    env: &Env<'_>,
    args: &[&str],
    part: &Path,
    expected: Option<u64>,
    progress: &dyn Progress,
) -> Result<std::process::Output, ImageError> {
    let mut command = Command::new(tool);
    command
        .args(args)
        .env_clear()
        .env("PATH", env.path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(home) = env.home {
        command.env("HOME", home);
    }
    let mut child = command.spawn()?;
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    let out_thread = std::thread::spawn(move || read_capped(stdout, 4 * 1024));
    let err_thread = std::thread::spawn(move || read_capped(stderr, 64 * 1024));
    loop {
        if progress.cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ImageError::Cancelled);
        }
        if let Some(len) = expected {
            if len > 0 {
                if let Ok(m) = fs::symlink_metadata(part) {
                    progress.file_step(((m.len().saturating_mul(1000) / len).min(1000)) as u32);
                }
            }
        }
        match child.try_wait()? {
            Some(status) => {
                // The child is gone; the pipes close on their own.
                return Ok(std::process::Output {
                    status,
                    stdout: out_thread.join().unwrap_or_default().into_bytes(),
                    stderr: err_thread.join().unwrap_or_default().into_bytes(),
                });
            }
            None => std::thread::sleep(Duration::from_millis(200)),
        }
    }
}

/// One image download. The address is checked again by the caller (`validate_url`).
///
/// The size is asked about first (a HEAD with a short timeout) so an oversized reply is refused
/// before it is downloaded; a reply without `Content-Length` is still capped by `--max-filesize`
/// and checked on disk before anything is published.
pub fn fetch_image(job: &ImageJob<'_>, progress: &dyn Progress) -> Result<PathBuf, ImageError> {
    if progress.cancelled() {
        return Err(ImageError::Cancelled);
    }
    // Ask first: a server that says the file is huge never gets a GET from us. A HEAD that fails
    // or says nothing usable is not fatal — the GET below carries its own limits. The announced
    // length also drives the progress bar (a reply without one still downloads, just ungraded).
    let mut expected: Option<u64> = None;
    let head = run_curl(
        job.tool,
        &job.env,
        &[
            "--proto", "=https", "--proto-redir", "=https",
            "--head", "--silent", "--location", "--max-redirs", "5",
            "--max-time", IMAGE_HEAD_MAX_TIME, "--write-out", "%{http_code}", "--", job.url,
        ],
    )?;
    if head.status.success() {
        let text = String::from_utf8_lossy(&head.stdout).to_ascii_lowercase();
        let last_len = text
            .lines()
            .filter_map(|l| l.trim().strip_prefix("content-length:"))
            .last()
            .and_then(|v| v.trim().parse::<u64>().ok());
        if let Some(len) = last_len {
            if len > IMAGE_MAX_BYTES {
                return Err(ImageError::TooLarge);
            }
            expected = Some(len);
        }
    }
    if progress.cancelled() {
        return Err(ImageError::Cancelled);
    }
    let work = WorkDir::create(job.work_parent)?;
    let part = work.root().join("part");
    let max_bytes = IMAGE_MAX_BYTES.to_string();
    let out = run_curl_progress(
        job.tool,
        &job.env,
        &[
            "--proto", "=https", "--proto-redir", "=https", "--silent", "--show-error",
            "--location", "--max-redirs", "5", "--max-time", IMAGE_MAX_TIME,
            "--max-filesize", &max_bytes, "--output", &part.to_string_lossy(),
            "--write-out", "%{http_code} %{content_type} %{size_download}", "--", job.url,
        ],
        &part,
        expected,
        progress,
    )?;
    if !out.status.success() {
        let tail = String::from_utf8_lossy(&out.stderr)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .rev()
            .take(2)
            .collect::<Vec<_>>()
            .join(" ");
        // curl 22 is the `--max-filesize` refusal (the server announced the size in the reply).
        if out.status.code() == Some(22) {
            return Err(ImageError::TooLarge);
        }
        return Err(ImageError::Failed(tail.chars().take(300).collect()));
    }
    let Some((code, ctype, size)) = parse_curl_report(&String::from_utf8_lossy(&out.stdout)) else {
        return Err(ImageError::NotImage);
    };
    if code != "200" {
        return Err(ImageError::NotImage);
    }
    let base = ctype.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    let Some(ext) = image_extension(&base) else {
        return Err(ImageError::NotImage);
    };
    if size > IMAGE_MAX_BYTES {
        return Err(ImageError::TooLarge);
    }
    let meta = fs::symlink_metadata(&part)?;
    if !meta.file_type().is_file() || meta.len() > IMAGE_MAX_BYTES {
        return Err(ImageError::TooLarge);
    }
    let mut head_bytes = vec![0u8; 12];
    let read = {
        use std::io::Read as _;
        let mut f = fs::File::open(&part)?;
        f.read(&mut head_bytes)?
    };
    if !image_header_matches(&head_bytes[..read], ext) {
        return Err(ImageError::NotImage);
    }
    let final_path = free_path(job.out_dir, &image_stem(job.url), ext);
    match fs::rename(&part, &final_path) {
        Ok(()) => {}
        // Across volumes rename fails; copy instead (the work folder is removed with the drop).
        Err(_) => {
            fs::copy(&part, &final_path)?;
            fs::remove_file(&part)?;
        }
    }
    Ok(final_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn image_is_an_offered_kind_only_without_a_height() {
        assert_eq!(kind_from("image", None), Some(Kind::Image));
        assert_eq!(kind_from("image", Some(720)), None);
        assert_eq!(kind_from("images", None), None);
    }

    #[test]
    fn image_names_never_replace_an_existing_file() {
        let dir = std::env::temp_dir().join(format!("flect-img-names-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        assert_eq!(free_path(&dir, "a", "png"), dir.join("a.png"));
        fs::write(dir.join("a.png"), b"x").unwrap();
        assert_eq!(free_path(&dir, "a", "png"), dir.join("a (2).png"));
        fs::write(dir.join("a (2).png"), b"x").unwrap();
        assert_eq!(free_path(&dir, "a", "png"), dir.join("a (3).png"));
        assert_eq!(free_path(&dir, "b", ""), dir.join("b"));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn image_stems_are_sanitized() {
        assert_eq!(image_stem("https://x.com/a/b/photo-1.png?size=large"), "photo-1");
        assert_eq!(image_stem("https://x.com/../etcpasswd"), "etcpasswd");
        assert_eq!(image_stem("https://x.com/.hidden"), "hidden");
        // Non-ASCII is dropped; a lone extension is not a name, so the host is used instead.
        assert_eq!(image_stem("https://x.com/日本語.png"), "x.com");
        assert_eq!(image_stem("https://x.com/日本語"), "x.com");
    }

    #[test]
    fn image_stem_falls_back_to_the_host() {
        assert_eq!(image_stem("https://example.com/日本語"), "example.com");
        assert_eq!(image_stem("https://example.com/"), "example.com");
    }

    #[test]
    fn image_headers_are_checked_by_bytes_not_by_the_content_type() {
        assert!(image_header_matches(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a], "png"));
        assert!(image_header_matches(b"\xff\xd8\xffabc", "jpg"));
        assert!(image_header_matches(b"GIF89a", "gif"));
        assert!(image_header_matches(b"RIFF\x04\x00\x00\x00WEBP", "webp"));
        assert!(image_header_matches(b"BM\x00\x00", "bmp"));
        assert!(image_header_matches(b"II*\0\0", "tif"));
        assert!(image_header_matches(b"<svg", "svg"));
        assert!(image_header_matches(b"\x00\x00\x00\x18ftypheic", "heic"));
        assert!(image_header_matches(b"\x00\x00\x00\x18ftypavif", "avif"));
        // A script answering "image/png" is refused.
        assert!(!image_header_matches(b"#!/bin/sh\n", "png"));
        assert!(!image_header_matches(b"RIFF\x04\x00\x00\x00AVI ", "webp"));
        assert!(!image_header_matches(b"\x00\x00\x00\x18ftypmsiv", "heic"));
        assert!(!image_header_matches(b"x", "jpg"));
    }

    #[test]
    fn image_stem_keeps_only_safe_ascii() {
        assert_eq!(image_stem("https://x.com/p/a;b`c|d.png"), "abcd");
    }

    #[test]
    fn curl_reports_are_parsed() {
        assert_eq!(
            parse_curl_report("200 image/png 1234"),
            Some(("200".into(), "image/png".into(), 1234))
        );
        assert_eq!(
            parse_curl_report("200 image/jpeg;charset=utf-8 99"),
            Some(("200".into(), "image/jpeg;charset=utf-8".into(), 99))
        );
        assert_eq!(parse_curl_report("200 image/png"), None);
        assert_eq!(parse_curl_report("200 image/png big"), None);
        assert_eq!(parse_curl_report(""), None);
    }

    #[test]
    fn image_preflight_needs_only_the_switch_and_curl() {
        let mut s = Settings::default();
        assert_eq!(preflight_image(&s, Some(PathBuf::from(CURL))), Err("download.disabled"));
        s.download.enabled = true;
        assert_eq!(preflight_image(&s, None), Err("download.toolMissing"));
        assert_eq!(
            preflight_image(&s, Some(PathBuf::from(CURL))),
            Ok(PathBuf::from(CURL))
        );
    }

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
            1,
        );
        assert_eq!(
            a,
            [
                "--ignore-config",
                "--no-playlist",
                "--playlist-items",
                "1",
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
                1,
            );
            assert!(a.contains(&"bv*+ba/b".to_string()), "{h:?}");
        }
    }

    #[test]
    fn playlist_items_stay_single_unless_asked() {
        let one = build_args(
            "https://example.com/v",
            Kind::Video { max_height: None },
            Path::new("/o"),
            Path::new("/w"),
            1,
        );
        assert!(one.windows(2).any(|w| w == ["--playlist-items", "1"]));
        let ten = build_args(
            "https://example.com/v",
            Kind::Video { max_height: None },
            Path::new("/o"),
            Path::new("/w"),
            10,
        );
        assert!(ten.windows(2).any(|w| w == ["--playlist-items", "1..10"]));
    }

    #[test]
    fn local_addresses_are_recognised_without_dns() {
        for u in [
            "http://127.0.0.1/x.png",
            "https://localhost:8080/x",
            "https://169.254.169.254/latest/meta-data/",
            "https://10.0.0.5/x",
            "https://172.16.0.1/x",
            "https://192.168.1.1/x",
            "https://100.126.0.1/x", // CGNAT
            "https://[::1]/x",
            "https://[::1]:8080/x",
            "https://[fe80::1]/x",
            "https://printer.local/x",
        ] {
            assert!(is_local_address(u), "{u}");
        }
        for u in ["https://example.com/x", "https://100.128.0.1/x", "https://8.8.8.8/x"] {
            assert!(!is_local_address(u), "{u}");
        }
    }

    #[test]
    fn heic_converts_to_png_when_sips_is_present() {
        // sips ships with macOS; elsewhere (or on a macOS that cannot encode HEIC) the test skips.
        if !Path::new(SIPS).exists() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("flect-sips-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let png = dir.join("seed.png");
        fs::write(
            &png,
            [
                137u8, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0,
                0, 1, 8, 6, 0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 218, 99,
                252, 207, 192, 80, 15, 0, 4, 133, 1, 128, 132, 169, 140, 33, 0, 0, 0, 0, 73, 69,
                78, 68, 174, 66, 96, 130,
            ],
        )
        .unwrap();
        let heic = dir.join("seed.heic");
        let encoded = Command::new(SIPS)
            .args([
                "-s", "format", "heic", &png.to_string_lossy(), "--out", &heic.to_string_lossy(),
            ])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        if !encoded.status.success() {
            let _ = fs::remove_dir_all(&dir);
            return;
        }
        let back = dir.join("back.png");
        convert_to_png(&heic, &back).expect("sips should convert HEIC to PNG");
        let bytes = fs::read(&back).unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn audio_arguments() {
        let a = build_args(
            "https://example.com/v",
            Kind::Audio,
            Path::new("/o"),
            Path::new("/w"),
            1,
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
                1,
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
                playlist_items: 1,
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
                playlist_items: 1,
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
                playlist_items: 1,
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
                    playlist_items: 1,
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
                    playlist_items: 1,
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
        // The kill is asynchronous: give the child a moment to finish dying. A zombie (waiting for a parent
        // that no longer exists) counts as gone.
        let alive = |pid: i32| -> bool {
            if let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) {
                return !stat.contains(") Z ");
            }
            // no /proc (macOS): ask ps; an empty answer means the process is gone
            Command::new("ps")
                .args(["-o", "stat=", "-p", &pid.to_string()])
                .output()
                .map(|o| {
                    let text = String::from_utf8_lossy(&o.stdout);
                    let text = text.trim();
                    !text.is_empty() && !text.starts_with('Z')
                })
                .unwrap_or(false)
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while alive(pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!alive(pid), "the child process survived the cancel");
    }
}
