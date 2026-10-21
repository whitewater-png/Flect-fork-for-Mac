//! ffmpeg / ffprobe plumbing: tool discovery, probing, encoder choice, running with progress and
//! cancellation. ffmpeg is a large C code base fed with untrusted files, so it runs as a separate
//! process with an argument vector (never a shell), absolute `file:` inputs, a protocol allow-list,
//! no stdin, and output paths that must not already exist.
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use crate::folder::Progress;
use crate::settings::{
    EncodePriority, HardwareEncoder, VideoCodecChoice, VideoContainer, VideoSettings,
};

#[derive(Debug)]
pub enum MediaError {
    NoTools,
    /// ffprobe failed or produced something unusable.
    Probe(String),
    /// A container we do not accept (playlists, network, filter graphs...).
    Unsupported(String),
    Ffmpeg {
        code: Option<i32>,
        tail: String,
    },
    NoVideo,
    NoAudio,
    NoDuration,
    Cancelled,
    /// ffmpeg was built without the whisper filter.
    NoWhisper,
    /// The pinned whisper model is not installed (the argument is the model file name).
    NoModel(String),
    /// The installed model does not match the pinned hash.
    ModelMismatch(String),
    /// Transcription produced no text.
    NoSpeech,
    /// The subtitle language setting is not a language code.
    BadLanguage,
    Io(std::io::Error),
}

impl MediaError {
    pub fn key(&self) -> &'static str {
        match self {
            Self::NoTools => "msg.ffmpegMissing",
            Self::Probe(_) => "msg.mediaProbeFailed",
            Self::Unsupported(m) if m == "trim.backwards" => "dialog.trim.backwards",
            Self::Unsupported(_) => "msg.mediaUnsupported",
            Self::Ffmpeg { .. } => "msg.ffmpegFailed",
            Self::NoVideo => "msg.noVideoStream",
            Self::NoAudio => "msg.noAudioTrack",
            Self::NoDuration => "msg.noDuration",
            Self::Cancelled => "msg.cancelled",
            Self::NoWhisper => "msg.whisperMissing",
            Self::NoModel(_) => "msg.whisperModelMissing",
            Self::ModelMismatch(_) => "msg.whisperModelMismatch",
            Self::NoSpeech => "msg.noSpeech",
            Self::BadLanguage => "msg.subtitleLanguageInvalid",
            Self::Io(e) => crate::paths::io_error_key(e),
        }
    }
    /// A crash (killed by a signal / abnormal exit) as opposed to a normal error exit.
    pub fn crashed(&self) -> bool {
        match self {
            Self::Ffmpeg { code: None, .. } => true,
            Self::Ffmpeg { code: Some(c), .. } => *c < 0 || *c > 128,
            _ => false,
        }
    }
}

impl From<std::io::Error> for MediaError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

// ------------------------------------------------------------------ tools

#[derive(Debug, Clone)]
pub struct Tools {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

impl Tools {
    /// Fixed install locations only (see `archive::find_tool`).
    pub fn discover() -> Option<Self> {
        Some(Self {
            ffmpeg: crate::archive::find_tool("ffmpeg")?,
            ffprobe: crate::archive::find_tool("ffprobe")?,
        })
    }
    /// Both tools from one directory, which must pass the same trust checks.
    pub fn from_dir(dir: &Path) -> Option<Self> {
        let (ffmpeg, ffprobe) = (dir.join("ffmpeg"), dir.join("ffprobe"));
        (crate::archive::tool_is_trustworthy(&ffmpeg)
            && crate::archive::tool_is_trustworthy(&ffprobe))
        .then_some(Self { ffmpeg, ffprobe })
    }
}

/// `file:` + absolute path. Refuses relative paths so ffmpeg never interprets a name as a URL or option.
pub fn file_arg(path: &Path) -> Result<String, MediaError> {
    let s = path
        .to_str()
        .ok_or_else(|| MediaError::Unsupported("non-UTF-8 path".into()))?;
    if !s.starts_with('/') || s.contains('\0') {
        return Err(MediaError::Unsupported("path must be absolute".into()));
    }
    Ok(format!("file:{s}"))
}

pub const PREAMBLE: [&str; 9] = [
    "-hide_banner",
    "-loglevel",
    "error",
    "-nostdin",
    "-progress",
    "pipe:1",
    "-protocol_whitelist",
    "file,pipe",
    "-n",
];

// ------------------------------------------------------------------ probing

#[derive(Debug, Clone, PartialEq)]
pub struct MediaInfo {
    pub duration_seconds: f64,
    pub width: u32,
    pub height: u32,
    pub frame_rate: f64,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub video_bitrate: u64,
    pub audio_bitrate: u64,
    pub audio_channels: u32,
    pub audio_tracks: u32,
    pub subtitle_tracks: u32,
    pub container: String,
}

impl MediaInfo {
    pub fn has_video(&self) -> bool {
        self.video_codec.is_some()
    }
    pub fn has_audio(&self) -> bool {
        self.audio_tracks > 0
    }
    pub fn short_side(&self) -> u32 {
        self.width.min(self.height)
    }
    pub fn is_mp4_family(&self) -> bool {
        let c = self.container.to_lowercase();
        c.contains("mp4") || c.contains("mov") || c.contains("m4a")
    }
}

/// Container names (ffprobe `format_name`) we are willing to open. Playlists (hls, concat),
/// network and generator demuxers are deliberately absent.
const ALLOWED_FORMATS: &[&str] = &[
    "mov",
    "mp4",
    "m4a",
    "3gp",
    "3g2",
    "mj2",
    "matroska",
    "webm",
    "avi",
    "flv",
    "mpeg",
    "mpegts",
    "mp3",
    "wav",
    "aiff",
    "ogg",
    "aac",
    "flac",
    "asf",
    "ac3",
    "amr",
    "ape",
    "wv",
    "caf",
    "gif",
    "dts",
    "eac3",
    "m4v",
    "ivf",
    "nut",
    "opus",
    "rm",
    "voc",
    "w64",
    "au",
    "mpegvideo",
    "h264",
    "hevc",
    "tta",
    "tak",
    "mlp",
    "truehd",
    "oma",
    "spdif",
    "swf",
    "vc1",
    "dv",
    "gxf",
    "mxf",
    "wtv",
    "ogv",
    "oga",
    "ogx",
    "ts",
    "asf_o",
];

pub fn format_allowed(format_name: &str) -> bool {
    !format_name.is_empty()
        && format_name
            .split(',')
            .all(|f| ALLOWED_FORMATS.contains(&f.trim()))
}

fn num(v: &serde_json::Value, key: &str) -> f64 {
    match v.get(key) {
        Some(serde_json::Value::String(s)) => s.parse().unwrap_or(0.0),
        Some(serde_json::Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        _ => 0.0,
    }
}
fn txt<'a>(v: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(|x| x.as_str())
}
fn rational(s: Option<&str>) -> f64 {
    let Some(s) = s else { return 0.0 };
    match s.split_once('/') {
        Some((n, d)) => {
            let (n, d) = (
                n.parse::<f64>().unwrap_or(0.0),
                d.parse::<f64>().unwrap_or(0.0),
            );
            if d > 0.0 {
                n / d
            } else {
                0.0
            }
        }
        None => s.parse().unwrap_or(0.0),
    }
}

pub fn parse_probe(json: &str) -> Result<MediaInfo, MediaError> {
    let root: serde_json::Value =
        serde_json::from_str(json).map_err(|e| MediaError::Probe(e.to_string()))?;
    let (mut duration, mut format_bitrate, mut container) = (0.0, 0.0, String::new());
    if let Some(f) = root.get("format") {
        duration = num(f, "duration");
        format_bitrate = num(f, "bit_rate");
        container = txt(f, "format_name").unwrap_or("").to_owned();
    }
    let mut info = MediaInfo {
        duration_seconds: if duration.is_finite() {
            duration.max(0.0)
        } else {
            0.0
        },
        width: 0,
        height: 0,
        frame_rate: 0.0,
        video_codec: None,
        audio_codec: None,
        video_bitrate: 0,
        audio_bitrate: 0,
        audio_channels: 0,
        audio_tracks: 0,
        subtitle_tracks: 0,
        container,
    };
    let mut rotation = 0i64;
    for s in root
        .get("streams")
        .and_then(|s| s.as_array())
        .into_iter()
        .flatten()
    {
        let codec = txt(s, "codec_name");
        match txt(s, "codec_type") {
            Some("video") => {
                if matches!(codec, Some("mjpeg" | "png" | "bmp" | "gif"))
                    || info.video_codec.is_some()
                {
                    continue; // cover art and the like
                }
                info.video_codec = codec.map(str::to_owned);
                info.width = num(s, "width") as u32;
                info.height = num(s, "height") as u32;
                info.frame_rate = rational(txt(s, "avg_frame_rate"));
                if info.frame_rate <= 0.0 {
                    info.frame_rate = rational(txt(s, "r_frame_rate"));
                }
                info.video_bitrate = num(s, "bit_rate") as u64;
                rotation = s
                    .get("side_data_list")
                    .and_then(|l| l.as_array())
                    .and_then(|l| {
                        l.iter()
                            .find_map(|d| d.get("rotation").and_then(|r| r.as_f64()))
                    })
                    .map(|r| r.round() as i64)
                    .or_else(|| {
                        s.get("tags")
                            .and_then(|t| txt(t, "rotate"))
                            .and_then(|r| r.parse().ok())
                    })
                    .unwrap_or(0);
            }
            Some("audio") => {
                info.audio_tracks += 1;
                if info.audio_codec.is_none() {
                    info.audio_codec = codec.map(str::to_owned);
                    info.audio_bitrate = num(s, "bit_rate") as u64;
                    info.audio_channels = num(s, "channels") as u32;
                }
            }
            Some("subtitle") => info.subtitle_tracks += 1,
            _ => {}
        }
    }
    if info.video_bitrate == 0 && format_bitrate > 0.0 {
        info.video_bitrate = (format_bitrate as u64).saturating_sub(info.audio_bitrate);
    }
    if rotation.rem_euclid(180) == 90 {
        std::mem::swap(&mut info.width, &mut info.height);
    }
    Ok(info)
}

/// Read a bounded amount of a child's stream (a hostile file must not make ffprobe's output unbounded).
fn read_capped(r: impl Read, cap: u64) -> String {
    let mut buf = Vec::new();
    let _ = r.take(cap).read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

pub fn probe(
    tools: &Tools,
    path: &Path,
    cancelled: &dyn Fn() -> bool,
) -> Result<MediaInfo, MediaError> {
    let mut child = Command::new(&tools.ffprobe)
        .args([
            "-v",
            "error",
            "-protocol_whitelist",
            "file",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
            "-i",
        ])
        .arg(file_arg(path)?)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    let err_thread = std::thread::spawn(move || read_capped(stderr, 64 * 1024));
    let out_thread = std::thread::spawn(move || read_capped(stdout, 8 << 20));
    let started = std::time::Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if cancelled() || started.elapsed() > Duration::from_secs(120) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(if cancelled() {
                MediaError::Cancelled
            } else {
                MediaError::Probe("timed out".into())
            });
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let (out, err) = (
        out_thread.join().unwrap_or_default(),
        err_thread.join().unwrap_or_default(),
    );
    if !status.success() {
        return Err(MediaError::Probe(
            err.lines().last().unwrap_or("ffprobe failed").to_owned(),
        ));
    }
    let info = parse_probe(&out)?;
    if !format_allowed(&info.container) {
        return Err(MediaError::Unsupported(info.container));
    }
    Ok(info)
}

// ------------------------------------------------------------------ running ffmpeg

/// `out_time_us=1234567` -> seconds.
pub fn parse_progress_line(line: &str) -> Option<f64> {
    let v: i64 = line.strip_prefix("out_time_us=")?.trim().parse().ok()?;
    (v >= 0).then(|| v as f64 / 1_000_000.0)
}

fn with_nice(tool: &Path) -> Command {
    let nice = Path::new("/usr/bin/nice");
    if nice.is_file() {
        let mut c = Command::new(nice);
        c.args(["-n", "5"]).arg(tool);
        c
    } else {
        Command::new(tool)
    }
}

/// Run ffmpeg to completion. `duration` (seconds, 0 = unknown) turns `-progress` output into
/// `progress.file_step(permille)`. Cancellation kills the process and returns `Cancelled`.
pub fn run_ffmpeg(
    tools: &Tools,
    args: &[String],
    duration: f64,
    progress: &dyn Progress,
) -> Result<(), MediaError> {
    run_ffmpeg_in(tools, args, duration, progress, None)
}

/// Like `run_ffmpeg`, with an optional working directory (for filters that take plain relative
/// names, which avoids escaping paths inside a filter graph).
pub fn run_ffmpeg_in(
    tools: &Tools,
    args: &[String],
    duration: f64,
    progress: &dyn Progress,
    cwd: Option<&Path>,
) -> Result<(), MediaError> {
    let mut command = with_nice(&tools.ffmpeg);
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    let mut child = command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    let err_thread = std::thread::spawn(move || read_capped(stderr, 64 * 1024));
    let (tx, rx) = mpsc::channel::<f64>();
    let out_thread = std::thread::spawn(move || {
        for line in BufReader::new(stdout.take(64 << 20))
            .lines()
            .map_while(Result::ok)
        {
            if let Some(t) = parse_progress_line(&line) {
                if tx.send(t).is_err() {
                    break;
                }
            }
        }
    });
    let status = loop {
        if progress.cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            let _ = out_thread.join();
            let _ = err_thread.join();
            return Err(MediaError::Cancelled);
        }
        while let Ok(t) = rx.try_recv() {
            if duration > 0.0 {
                progress.file_step(((t / duration).clamp(0.0, 1.0) * 1000.0) as u32);
            }
        }
        match child.try_wait()? {
            Some(s) => break s,
            None => std::thread::sleep(Duration::from_millis(40)),
        }
    };
    let _ = out_thread.join();
    let err = err_thread.join().unwrap_or_default();
    if status.success() {
        Ok(())
    } else {
        use std::os::unix::process::ExitStatusExt;
        let tail = err
            .lines()
            .rev()
            .take(3)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join(" ");
        // A signal death has no exit code; report it as a crash.
        Err(MediaError::Ffmpeg {
            code: status.code().or(status.signal().map(|s| -s)),
            tail,
        })
    }
}

// ------------------------------------------------------------------ encoders

/// Does this ffmpeg build have the named filter? (`whisper` exists only in builds such as Homebrew's `ffmpeg-full`.)
pub fn has_filter(tools: &Tools, name: &str) -> bool {
    let Ok(out) = Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-filters"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return false;
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .any(|l| l.split_whitespace().nth(1) == Some(name))
}

pub fn list_encoders(tools: &Tools) -> HashSet<String> {
    let mut names = HashSet::new();
    let Ok(out) = Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-encoders"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return names;
    };
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let t = line.trim_start();
        if t.len() < 8 || !matches!(t.as_bytes()[0], b'V' | b'A' | b'S') {
            continue;
        }
        if let Some(name) = t.split_whitespace().nth(1) {
            names.insert(name.to_owned());
        }
    }
    names
}

/// Can this machine's hardware encoder really encode? (Listed does not mean working.)
pub fn hardware_works(tools: &Tools, encoder: &str) -> bool {
    let Ok(mut child) = Command::new(&tools.ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=320x240:d=1",
            "-c:v",
            encoder,
            "-f",
            "null",
            "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let t = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(s)) => return s.success(),
            Ok(None) if t.elapsed() < Duration::from_secs(15) => {
                std::thread::sleep(Duration::from_millis(50))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncoderChoice {
    pub name: String,
    pub hardware: bool,
}

/// What the installed ffmpeg offers, plus whether hardware was proven to work.
#[derive(Debug, Clone, Default)]
pub struct Capabilities {
    pub encoders: HashSet<String>,
    pub videotoolbox_works: bool,
}

impl Capabilities {
    pub fn detect(tools: &Tools, want_hardware: bool) -> Self {
        let encoders = list_encoders(tools);
        let videotoolbox_works = want_hardware
            && encoders.contains("h264_videotoolbox")
            && hardware_works(tools, "h264_videotoolbox");
        Self {
            encoders,
            videotoolbox_works,
        }
    }
    fn has(&self, name: &str) -> bool {
        self.encoders.contains(name)
    }

    pub fn software(&self, codec: VideoCodecChoice) -> EncoderChoice {
        let name = match codec {
            VideoCodecChoice::H265 => "libx265",
            VideoCodecChoice::Vp9 => "libvpx-vp9",
            VideoCodecChoice::Av1 => {
                if self.has("libsvtav1") {
                    "libsvtav1"
                } else {
                    "libaom-av1"
                }
            }
            VideoCodecChoice::H264 => "libx264",
        };
        EncoderChoice {
            name: if self.has(name) { name } else { "libx264" }.to_owned(),
            hardware: false,
        }
    }

    pub fn preferred(
        &self,
        codec: VideoCodecChoice,
        hw: HardwareEncoder,
        hw_disabled: bool,
    ) -> EncoderChoice {
        if hw == HardwareEncoder::VideoToolbox && self.videotoolbox_works && !hw_disabled {
            let name = match codec {
                VideoCodecChoice::H265 => Some("hevc_videotoolbox"),
                VideoCodecChoice::H264 => Some("h264_videotoolbox"),
                _ => None, // no VideoToolbox VP9/AV1 encoder
            };
            if let Some(n) = name.filter(|n| self.has(n)) {
                return EncoderChoice {
                    name: n.to_owned(),
                    hardware: true,
                };
            }
        }
        self.software(codec)
    }

    pub fn for_container(&self, ext: &str, v: &VideoSettings, hw_disabled: bool) -> EncoderChoice {
        match ext {
            ".webm" => self.preferred(
                codec_for(VideoContainer::WebM, v.codec),
                v.hardware,
                hw_disabled,
            ),
            ".avi" => EncoderChoice {
                name: "mpeg4".into(),
                hardware: false,
            },
            ".mov" => self.preferred(
                if matches!(v.codec, VideoCodecChoice::H264 | VideoCodecChoice::H265) {
                    v.codec
                } else {
                    VideoCodecChoice::H264
                },
                v.hardware,
                hw_disabled,
            ),
            _ => self.preferred(v.codec, v.hardware, hw_disabled),
        }
    }

    /// Software equivalent of a hardware choice, used when the hardware encode fails.
    pub fn cpu_fallback(&self, e: &EncoderChoice) -> Option<EncoderChoice> {
        e.hardware.then(|| {
            self.software(if e.name.starts_with("hevc") {
                VideoCodecChoice::H265
            } else {
                VideoCodecChoice::H264
            })
        })
    }
}

pub fn codec_for(container: VideoContainer, codec: VideoCodecChoice) -> VideoCodecChoice {
    if container == VideoContainer::WebM
        && !matches!(codec, VideoCodecChoice::Vp9 | VideoCodecChoice::Av1)
    {
        VideoCodecChoice::Vp9
    } else {
        codec
    }
}

fn level(p: EncodePriority, speed: u32, balanced: u32, quality: u32) -> String {
    match p {
        EncodePriority::Speed => speed,
        EncodePriority::Balanced => balanced,
        EncodePriority::Quality => quality,
    }
    .to_string()
}
fn sw_preset(p: EncodePriority) -> &'static str {
    match p {
        EncodePriority::Speed => "veryfast",
        EncodePriority::Balanced => "medium",
        EncodePriority::Quality => "slow",
    }
}
fn av1_preset(p: EncodePriority) -> &'static str {
    match p {
        EncodePriority::Speed => "10",
        EncodePriority::Balanced => "8",
        EncodePriority::Quality => "6",
    }
}
fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| (*x).to_owned()).collect()
}

/// Constant-quality arguments (resize, rotate, convert, exact trim).
pub fn quality_args(e: &EncoderChoice, p: EncodePriority) -> Vec<String> {
    let mut a = s(&["-c:v", &e.name]);
    if e.hardware {
        // VideoToolbox takes a 0-100 quality (higher is better).
        a.extend(s(&["-q:v", &level(p, 50, 60, 72)]));
        return a;
    }
    match e.name.as_str() {
        "libvpx-vp9" => a.extend(s(&["-crf", &level(p, 36, 32, 28), "-b:v", "0"])),
        "libsvtav1" | "libaom-av1" => a.extend(s(&[
            "-crf",
            &level(p, 38, 34, 30),
            "-preset",
            av1_preset(p),
        ])),
        "mpeg4" => a.extend(s(&["-qscale:v", &level(p, 5, 3, 2)])),
        _ => a.extend(s(&["-crf", &level(p, 24, 20, 18), "-preset", sw_preset(p)])),
    }
    a
}

/// Target-bitrate arguments (size-limited compression).
pub fn bitrate_args(e: &EncoderChoice, kbps: u32, p: EncodePriority) -> Vec<String> {
    let mut a = s(&["-c:v", &e.name, "-b:v", &format!("{kbps}k")]);
    if e.hardware {
        return a;
    }
    match e.name.as_str() {
        "libsvtav1" | "libaom-av1" => a.extend(s(&["-preset", av1_preset(p)])),
        "libvpx-vp9" => a.extend(s(&[
            "-deadline",
            if p == EncodePriority::Speed {
                "realtime"
            } else {
                "good"
            },
        ])),
        "mpeg4" => {}
        _ => a.extend(s(&["-preset", sw_preset(p)])),
    }
    a
}

pub fn pixel_format_args(e: &EncoderChoice) -> Vec<String> {
    if e.hardware {
        vec![]
    } else {
        s(&["-pix_fmt", "yuv420p"])
    }
}

/// HEVC in MP4/MOV needs the `hvc1` tag or QuickTime/Safari will not play it.
pub fn tag_args(e: &EncoderChoice, ext: &str) -> Vec<String> {
    if (e.name.contains("265") || e.name.starts_with("hevc"))
        && matches!(ext, ".mp4" | ".mov" | ".m4v")
    {
        s(&["-tag:v", "hvc1"])
    } else {
        vec![]
    }
}

pub fn audio_codec_args(ext: &str) -> Vec<String> {
    match ext {
        ".webm" => s(&["-c:a", "libopus"]),
        ".avi" => s(&["-c:a", "libmp3lame"]),
        _ => s(&["-c:a", "aac"]),
    }
}
pub fn audio_encoder_args(ext: &str) -> Vec<String> {
    let mut a = audio_codec_args(ext);
    a.extend(s(&["-b:a", "192k"]));
    a
}

pub fn make_even(v: u32) -> u32 {
    (v / 2 * 2).max(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "streams": [
        {"codec_type":"video","codec_name":"mjpeg","width":100,"height":100},
        {"codec_type":"video","codec_name":"h264","width":1920,"height":1080,"avg_frame_rate":"30000/1001","bit_rate":"4000000",
         "side_data_list":[{"rotation":-90}]},
        {"codec_type":"audio","codec_name":"aac","bit_rate":"128000","channels":2},
        {"codec_type":"audio","codec_name":"aac","channels":1},
        {"codec_type":"subtitle","codec_name":"mov_text"}
      ],
      "format": {"format_name":"mov,mp4,m4a,3gp,3g2,mj2","duration":"12.5","bit_rate":"4200000"}
    }"#;

    #[test]
    fn parses_ffprobe_json() {
        let i = parse_probe(SAMPLE).unwrap();
        assert_eq!(
            (i.width, i.height),
            (1080, 1920),
            "rotation swaps the dimensions"
        );
        assert_eq!(
            (i.video_codec.as_deref(), i.audio_codec.as_deref()),
            (Some("h264"), Some("aac"))
        );
        assert_eq!(
            (i.audio_tracks, i.subtitle_tracks, i.audio_channels),
            (2, 1, 2)
        );
        assert!((i.frame_rate - 29.97).abs() < 0.01 && (i.duration_seconds - 12.5).abs() < 1e-9);
        assert_eq!((i.video_bitrate, i.audio_bitrate), (4_000_000, 128_000));
        assert!(i.is_mp4_family() && i.has_video() && i.has_audio() && i.short_side() == 1080);
    }

    #[test]
    fn probe_handles_odd_inputs() {
        assert!(parse_probe("not json").is_err());
        let empty = parse_probe("{}").unwrap();
        assert!(!empty.has_video() && empty.duration_seconds == 0.0);
        let nan = parse_probe(r#"{"format":{"duration":"NaN"}}"#).unwrap();
        assert_eq!(nan.duration_seconds, 0.0);
        let legacy = parse_probe(r#"{"streams":[{"codec_type":"video","codec_name":"h264","width":640,"height":360,"tags":{"rotate":"270"}}]}"#).unwrap();
        assert_eq!((legacy.width, legacy.height), (360, 640));
        // format bitrate fills in a missing video bitrate
        let br = parse_probe(r#"{"streams":[{"codec_type":"video","codec_name":"h264","width":2,"height":2},{"codec_type":"audio","codec_name":"aac","bit_rate":"100"}],"format":{"bit_rate":"1000"}}"#).unwrap();
        assert_eq!(br.video_bitrate, 900);
    }

    #[test]
    fn container_allow_list() {
        assert!(
            format_allowed("mov,mp4,m4a,3gp,3g2,mj2")
                && format_allowed("matroska,webm")
                && format_allowed("mp3")
        );
        for bad in [
            "hls",
            "concat",
            "lavfi",
            "rtsp",
            "sdp",
            "image2",
            "tee",
            "mov,hls",
            "",
            "applehttp",
        ] {
            assert!(!format_allowed(bad), "{bad}");
        }
    }

    #[test]
    fn file_args_are_absolute_only() {
        assert_eq!(
            file_arg(Path::new("/tmp/a b.mp4")).unwrap(),
            "file:/tmp/a b.mp4"
        );
        assert!(file_arg(Path::new("rel.mp4")).is_err());
        assert!(file_arg(Path::new("-i")).is_err());
        assert!(file_arg(Path::new("http://x/y.mp4")).is_err());
    }

    #[test]
    fn progress_lines() {
        assert_eq!(parse_progress_line("out_time_us=1500000"), Some(1.5));
        assert_eq!(parse_progress_line("out_time_us=-5"), None);
        assert_eq!(parse_progress_line("out_time_us=N/A"), None);
        assert_eq!(parse_progress_line("frame=10"), None);
        assert!(
            PREAMBLE.contains(&"-nostdin")
                && PREAMBLE
                    .windows(2)
                    .any(|w| w == ["-protocol_whitelist", "file,pipe"])
                && PREAMBLE.contains(&"-n")
        );
    }

    fn caps(names: &[&str], vt: bool) -> Capabilities {
        Capabilities {
            encoders: names.iter().map(|n| (*n).to_owned()).collect(),
            videotoolbox_works: vt,
        }
    }

    #[test]
    fn encoder_choice_prefers_proven_hardware() {
        let c = caps(
            &[
                "libx264",
                "libx265",
                "h264_videotoolbox",
                "hevc_videotoolbox",
                "libvpx-vp9",
                "mpeg4",
            ],
            true,
        );
        let hw = |codec| c.preferred(codec, HardwareEncoder::VideoToolbox, false);
        assert_eq!(
            hw(VideoCodecChoice::H264),
            EncoderChoice {
                name: "h264_videotoolbox".into(),
                hardware: true
            }
        );
        assert_eq!(hw(VideoCodecChoice::H265).name, "hevc_videotoolbox");
        assert!(!hw(VideoCodecChoice::Vp9).hardware, "no VideoToolbox VP9");
        assert!(
            !c.preferred(VideoCodecChoice::H264, HardwareEncoder::None, false)
                .hardware
        );
        assert!(
            !c.preferred(VideoCodecChoice::H264, HardwareEncoder::VideoToolbox, true)
                .hardware,
            "disabled after a failure"
        );
        assert!(
            !caps(&["libx264", "h264_videotoolbox"], false)
                .preferred(VideoCodecChoice::H264, HardwareEncoder::VideoToolbox, false)
                .hardware,
            "listed but not proven to work"
        );
        assert_eq!(
            c.cpu_fallback(&hw(VideoCodecChoice::H265)).unwrap().name,
            "libx265"
        );
        assert!(c
            .cpu_fallback(&c.software(VideoCodecChoice::H264))
            .is_none());
        // missing encoders fall back to libx264
        assert_eq!(
            caps(&["libx264"], false)
                .software(VideoCodecChoice::Av1)
                .name,
            "libx264"
        );
    }

    #[test]
    fn containers_pick_suitable_codecs() {
        let c = caps(&["libx264", "libx265", "libvpx-vp9", "mpeg4"], false);
        let v = VideoSettings::default();
        assert_eq!(c.for_container(".webm", &v, false).name, "libvpx-vp9");
        assert_eq!(c.for_container(".avi", &v, false).name, "mpeg4");
        let vp9 = VideoSettings {
            codec: VideoCodecChoice::Vp9,
            ..Default::default()
        };
        assert_eq!(
            c.for_container(".mov", &vp9, false).name,
            "libx264",
            "MOV cannot hold VP9"
        );
        assert_eq!(c.for_container(".mp4", &v, false).name, "libx264");
    }

    #[test]
    fn encoder_arguments() {
        let sw = EncoderChoice {
            name: "libx264".into(),
            hardware: false,
        };
        let hw = EncoderChoice {
            name: "h264_videotoolbox".into(),
            hardware: true,
        };
        assert_eq!(
            quality_args(&sw, EncodePriority::Balanced),
            ["-c:v", "libx264", "-crf", "20", "-preset", "medium"]
        );
        assert_eq!(
            quality_args(&hw, EncodePriority::Quality),
            ["-c:v", "h264_videotoolbox", "-q:v", "72"]
        );
        assert_eq!(
            bitrate_args(&sw, 1500, EncodePriority::Speed),
            ["-c:v", "libx264", "-b:v", "1500k", "-preset", "veryfast"]
        );
        assert_eq!(
            bitrate_args(&hw, 1500, EncodePriority::Speed),
            ["-c:v", "h264_videotoolbox", "-b:v", "1500k"]
        );
        assert_eq!(pixel_format_args(&sw), ["-pix_fmt", "yuv420p"]);
        assert!(pixel_format_args(&hw).is_empty());
        let x265 = EncoderChoice {
            name: "libx265".into(),
            hardware: false,
        };
        assert_eq!(tag_args(&x265, ".mp4"), ["-tag:v", "hvc1"]);
        assert!(tag_args(&x265, ".mkv").is_empty() && tag_args(&sw, ".mp4").is_empty());
        assert_eq!(
            audio_encoder_args(".webm"),
            ["-c:a", "libopus", "-b:a", "192k"]
        );
        assert_eq!(audio_codec_args(".avi"), ["-c:a", "libmp3lame"]);
        assert_eq!(make_even(1), 2);
        assert_eq!(make_even(1081), 1080);
    }
}
