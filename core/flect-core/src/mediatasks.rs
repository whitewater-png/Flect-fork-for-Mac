//! Video and audio tasks built on `media`: resize, rotate, convert, trim, thumbnail,
//! extract audio, and size-targeted compression (ported from the Windows pipeline:
//! remux -> re-encode audio only -> re-encode video, correcting the bitrate once).
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::folder::Progress;
use crate::imaging::parse_hex_color;
use crate::media::{self, Capabilities, EncoderChoice, MediaError, MediaInfo, Tools};
use crate::paths;
use crate::settings::{Settings, ThumbnailFormat, TrimAccuracy, VideoCodecChoice, VideoContainer};
use crate::tasks::Done;
use crate::workspace::{commit_new, safe_delete, ScratchDirectory};

pub fn is_media_task(name: &str) -> bool {
    matches!(
        name,
        "video.resize"
            | "video.rotate"
            | "video.convert"
            | "video.trim"
            | "video.thumbnail"
            | "video.compress"
            | "video.extractAudio"
            | "audio.convert"
            | "video.subtitles"
            | "audio.subtitles"
    )
}

pub struct MediaEnv {
    pub tools: Tools,
    pub caps: Capabilities,
}

pub struct Job<'a> {
    pub env: &'a MediaEnv,
    pub settings: &'a Settings,
    pub progress: &'a dyn Progress,
    hw_disabled: Cell<bool>,
    warnings: RefCell<Vec<(&'static str, Vec<String>)>>,
    infos: RefCell<Vec<(&'static str, Vec<String>)>>,
}

impl<'a> Job<'a> {
    pub fn new(env: &'a MediaEnv, settings: &'a Settings, progress: &'a dyn Progress) -> Self {
        Self {
            env,
            settings,
            progress,
            hw_disabled: Cell::new(false),
            warnings: RefCell::default(),
            infos: RefCell::default(),
        }
    }
    fn warn(&self, key: &'static str, args: Vec<String>) {
        self.warnings.borrow_mut().push((key, args));
    }
    fn info(&self, key: &'static str, args: Vec<String>) {
        self.infos.borrow_mut().push((key, args));
    }
    fn check(&self) -> Result<(), MediaError> {
        if self.progress.cancelled() {
            Err(MediaError::Cancelled)
        } else {
            Ok(())
        }
    }
    fn probe(&self, path: &Path) -> Result<MediaInfo, MediaError> {
        media::probe(&self.env.tools, path, &|| self.progress.cancelled())
    }
    fn ffmpeg(&self, args: &[String], duration: f64) -> Result<(), MediaError> {
        media::run_ffmpeg(&self.env.tools, args, duration, self.progress)
    }
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| (*x).to_owned()).collect()
}

/// `["-hide_banner", ..., "-i", file:IN] ++ rest ++ [file:OUT]`
fn command(input: &Path, middle: Vec<String>, output: &Path) -> Result<Vec<String>, MediaError> {
    let mut a = s(&media::PREAMBLE);
    a.push("-i".into());
    a.push(media::file_arg(input)?);
    a.extend(middle);
    a.push(media::file_arg(output)?);
    Ok(a)
}

fn ext_of(path: &Path) -> String {
    path.extension()
        .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
        .unwrap_or_default()
}

fn threads_args(t: Option<u32>) -> Vec<String> {
    t.map(|n| vec!["-threads".to_owned(), n.to_string()])
        .unwrap_or_default()
}

type BuildFn<'a> = &'a dyn Fn(&EncoderChoice, Option<u32>) -> Result<Vec<String>, MediaError>;

/// Run an encode: hardware first when chosen, software if that fails, and on a crash retry
/// with fewer threads. `build` produces the full argument list for (encoder, threads).
fn run_encode(
    job: &Job<'_>,
    staged: &Path,
    ext: &str,
    duration: f64,
    build: BuildFn<'_>,
) -> Result<(), MediaError> {
    let video = &job.settings.video;
    let first = job
        .env
        .caps
        .for_container(ext, video, job.hw_disabled.get());
    let attempt = |enc: &EncoderChoice| -> Result<(), MediaError> {
        let ladder = [None, Some(4), Some(1)];
        for (i, t) in ladder.iter().enumerate() {
            safe_delete(staged);
            match job.ffmpeg(&build(enc, *t)?, duration) {
                Err(e) if e.crashed() && i + 1 < ladder.len() => continue,
                other => return other,
            }
        }
        unreachable!("the ladder always returns")
    };
    match attempt(&first) {
        Err(MediaError::Cancelled) => Err(MediaError::Cancelled),
        Err(e) if first.hardware => {
            job.hw_disabled.set(true);
            job.warn("msg.hardwareFallback", vec![first.name.clone()]);
            let fb = job.env.caps.cpu_fallback(&first).ok_or(e)?;
            attempt(&fb)
        }
        other => other,
    }
}

/// Keep the audio as is; if the container cannot take it, encode it.
fn with_audio_fallback(
    staged: &Path,
    mut run: impl FnMut(Vec<String>) -> Result<(), MediaError>,
    ext: &str,
) -> Result<(), MediaError> {
    match run(s(&["-c:a", "copy"])) {
        Err(MediaError::Cancelled) => Err(MediaError::Cancelled),
        Err(MediaError::Ffmpeg { .. }) => {
            safe_delete(staged);
            run(media::audio_encoder_args(ext))
        }
        other => other,
    }
}

/// Move a finished staged file next to `source` under a fresh name.
fn publish(
    staged: &Path,
    source: &Path,
    suffix: &str,
    ext: Option<&str>,
) -> Result<PathBuf, MediaError> {
    for _ in 0..16 {
        let target = paths::derive(source, suffix, ext);
        match commit_new(staged, &target) {
            Ok(()) => return Ok(target),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(MediaError::Io(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free output name",
    )))
}

fn param_f64(p: &HashMap<String, String>, k: &str) -> Option<f64> {
    p.get(k)
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| v.is_finite())
}

fn require_video(info: &MediaInfo) -> Result<(), MediaError> {
    if info.has_video() {
        Ok(())
    } else {
        Err(MediaError::NoVideo)
    }
}

// ------------------------------------------------------------------ filters

const EVEN: &str = "scale=trunc(iw/2)*2:trunc(ih/2)*2";

pub fn scale_filter(params: &HashMap<String, String>) -> String {
    if params
        .get("scale")
        .is_some_and(|v| v.eq_ignore_ascii_case("custom"))
    {
        let even = |v: f64| {
            let v = v.clamp(0.0, 16384.0) as u32;
            v - v % 2
        };
        let (w, h) = (
            even(param_f64(params, "w").unwrap_or(0.0)),
            even(param_f64(params, "h").unwrap_or(0.0)),
        );
        let keep = params.get("keep").is_none_or(|v| v != "0");
        return match (w, h) {
            (0, 0) => "scale=iw:ih".to_owned(),
            (w, 0) => format!("scale={w}:-2"),
            (0, h) => format!("scale=-2:{h}"),
            (w, h) if keep => format!("scale={w}:{h}:force_original_aspect_ratio=decrease,{EVEN}"),
            (w, h) => format!("scale={w}:{h}"),
        };
    }
    let pct = param_f64(params, "scale")
        .filter(|v| *v > 0.0 && *v <= 1000.0)
        .unwrap_or(50.0);
    let factor = pct / 100.0;
    format!("scale=trunc(iw*{factor}/2)*2:trunc(ih*{factor}/2)*2")
}

pub fn rotate_filter(deg: f64, fill: &str) -> String {
    let turn = deg.rem_euclid(360.0);
    if turn == 90.0 {
        return format!("transpose=1,{EVEN}");
    }
    if turn == 180.0 {
        return format!("transpose=1,transpose=1,{EVEN}");
    }
    if turn == 270.0 {
        return format!("transpose=2,{EVEN}");
    }
    let color = parse_hex_color(fill).unwrap_or([0, 0, 0]);
    let angle = format!("{}*PI/180", deg.clamp(-3600.0, 3600.0));
    format!(
        "rotate={angle}:ow=rotw({angle}):oh=roth({angle}):fillcolor=0x{:02X}{:02X}{:02X},{EVEN}",
        color[0], color[1], color[2]
    )
}

// ------------------------------------------------------------------ simple tasks

fn filter_task(
    job: &Job<'_>,
    input: &Path,
    suffix: &str,
    filter: &str,
) -> Result<PathBuf, MediaError> {
    let info = job.probe(input)?;
    require_video(&info)?;
    let ext = ext_of(input);
    let scratch = ScratchDirectory::new(input)?;
    let staged = scratch.path_for(&format!("out{ext}"));
    let video = &job.settings.video;
    with_audio_fallback(
        &staged,
        |audio| {
            run_encode(job, &staged, &ext, info.duration_seconds, &|enc, t| {
                let mut mid = s(&["-vf", filter]);
                mid.extend(threads_args(t));
                mid.extend(media::quality_args(enc, video.priority));
                mid.extend(media::pixel_format_args(enc));
                mid.extend(media::tag_args(enc, &ext));
                mid.extend(audio.clone());
                command(input, mid, &staged)
            })
        },
        &ext,
    )?;
    publish(&staged, input, suffix, None)
}

fn gif_chain(job: &Job<'_>) -> String {
    let v = &job.settings.video;
    format!(
        "fps={},scale='min({},iw)':-2:flags=lanczos",
        v.gif_fps.clamp(1, 60),
        v.gif_max_width.clamp(16, 4096)
    )
}

fn palette_use(job: &Job<'_>) -> &'static str {
    use crate::settings::GifDither::*;
    match job.settings.video.gif_dither {
        None => "paletteuse=dither=none:diff_mode=rectangle",
        FloydSteinberg => "paletteuse=dither=floyd_steinberg:diff_mode=rectangle",
        Bayer => "paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle",
    }
}

fn to_gif(job: &Job<'_>, input: &Path, info: &MediaInfo) -> Result<PathBuf, MediaError> {
    let scratch = ScratchDirectory::new(input)?;
    let (palette, staged) = (scratch.path_for("palette.png"), scratch.path_for("out.gif"));
    let chain = gif_chain(job);
    job.ffmpeg(
        &command(
            input,
            s(&["-vf", &format!("{chain},palettegen=stats_mode=diff")]),
            &palette,
        )?,
        info.duration_seconds,
    )?;
    let mut a = s(&media::PREAMBLE);
    a.extend(s(&[
        "-i",
        &media::file_arg(input)?,
        "-i",
        &media::file_arg(&palette)?,
        "-lavfi",
        &format!("{chain}[x];[x][1:v]{}", palette_use(job)),
        "-loop",
        "0",
    ]));
    a.push(media::file_arg(&staged)?);
    job.ffmpeg(&a, info.duration_seconds)?;
    publish(&staged, input, "", Some("gif"))
}

fn convert(job: &Job<'_>, input: &Path, target: &str) -> Result<PathBuf, MediaError> {
    let info = job.probe(input)?;
    if target == "GIF" {
        require_video(&info)?;
        return to_gif(job, input, &info);
    }
    let ext = format!(".{}", target.to_lowercase());
    let m4a = target == "M4A";
    let scratch = ScratchDirectory::new(input)?;
    let staged = scratch.path_for(&format!("out{ext}"));
    // 1. try to repackage without re-encoding
    let copy = if m4a {
        s(&["-vn", "-c:a", "copy"])
    } else {
        s(&["-c", "copy"])
    };
    match job.ffmpeg(&command(input, copy, &staged)?, info.duration_seconds) {
        Ok(()) => return publish(&staged, input, "", Some(&ext)),
        Err(MediaError::Ffmpeg { .. }) => safe_delete(&staged),
        Err(e) => return Err(e),
    }
    // 2. re-encode
    if m4a {
        let mut mid = s(&["-vn"]);
        mid.extend(media::audio_encoder_args(&ext));
        job.ffmpeg(&command(input, mid, &staged)?, info.duration_seconds)?;
    } else {
        require_video(&info)?;
        let video = &job.settings.video;
        run_encode(job, &staged, &ext, info.duration_seconds, &|enc, t| {
            let mut mid = threads_args(t);
            mid.extend(media::quality_args(enc, video.priority));
            mid.extend(media::pixel_format_args(enc));
            mid.extend(media::tag_args(enc, &ext));
            mid.extend(media::audio_encoder_args(&ext));
            command(input, mid, &staged)
        })?;
    }
    publish(&staged, input, "", Some(&ext))
}

fn trim(
    job: &Job<'_>,
    input: &Path,
    params: &HashMap<String, String>,
) -> Result<PathBuf, MediaError> {
    let info = job.probe(input)?;
    require_video(&info)?;
    if info.duration_seconds <= 0.0 {
        return Err(MediaError::NoDuration);
    }
    let start = param_f64(params, "start")
        .unwrap_or(0.0)
        .clamp(0.0, (info.duration_seconds - 0.05).max(0.0));
    let end = param_f64(params, "end")
        .unwrap_or(info.duration_seconds)
        .clamp(start, info.duration_seconds);
    let length = end - start;
    if length <= 0.0 {
        return Err(MediaError::Unsupported("trim.backwards".into()));
    }
    let ext = ext_of(input);
    let scratch = ScratchDirectory::new(input)?;
    let staged = scratch.path_for(&format!("out{ext}"));
    let seek = |mid: Vec<String>| -> Result<Vec<String>, MediaError> {
        let mut a = s(&media::PREAMBLE);
        a.extend(s(&[
            "-ss",
            &format!("{start:.3}"),
            "-i",
            &media::file_arg(input)?,
            "-t",
            &format!("{length:.3}"),
        ]));
        a.extend(mid);
        a.push(media::file_arg(&staged)?);
        Ok(a)
    };
    if job.settings.video.trim.accuracy == TrimAccuracy::Keyframe {
        job.ffmpeg(
            &seek(s(&["-c", "copy", "-avoid_negative_ts", "make_zero"]))?,
            length,
        )?;
    } else {
        let video = &job.settings.video;
        run_encode(job, &staged, &ext, length, &|enc, t| {
            let mut mid = threads_args(t);
            mid.extend(media::quality_args(enc, video.priority));
            mid.extend(media::pixel_format_args(enc));
            mid.extend(media::tag_args(enc, &ext));
            mid.extend(media::audio_encoder_args(&ext));
            seek(mid)
        })?;
    }
    publish(&staged, input, "_trimmed", None)
}

fn thumbnail(
    job: &Job<'_>,
    input: &Path,
    params: &HashMap<String, String>,
) -> Result<PathBuf, MediaError> {
    let info = job.probe(input)?;
    require_video(&info)?;
    let t = param_f64(params, "t")
        .unwrap_or(0.0)
        .clamp(0.0, info.duration_seconds.max(0.0));
    let th = &job.settings.video.thumbnail;
    let ext = if th.format == ThumbnailFormat::Jpg {
        "jpg"
    } else {
        "png"
    };
    let scratch = ScratchDirectory::new(input)?;
    let staged = scratch.path_for(&format!("out.{ext}"));
    let mut a = s(&media::PREAMBLE);
    a.extend(s(&[
        "-ss",
        &format!("{t:.3}"),
        "-i",
        &media::file_arg(input)?,
        "-frames:v",
        "1",
    ]));
    if th.max_width > 0 {
        a.extend(s(&[
            "-vf",
            &format!("scale='min({},iw)':-2", th.max_width.min(16384)),
        ]));
    }
    if th.format == ThumbnailFormat::Jpg {
        a.extend(s(&[
            "-q:v",
            &(31 - th.quality.clamp(1, 100) * 29 / 100).to_string(),
        ]));
    }
    a.push(media::file_arg(&staged)?);
    job.ffmpeg(&a, 0.0)?;
    publish(&staged, input, "_thumbnail", Some(ext))
}

/// Transcribe the audio with ffmpeg's whisper filter and write `name.srt` / `.vtt` / `.txt` next to the source.
fn subtitles(job: &Job<'_>, input: &Path) -> Result<PathBuf, MediaError> {
    use crate::settings::SubtitleDevice;
    use crate::settings::SubtitleOutput;
    let info = job.probe(input)?;
    if !info.has_audio() {
        return Err(MediaError::NoAudio);
    }
    if !media::has_filter(&job.env.tools, "whisper") {
        return Err(MediaError::NoWhisper);
    }
    let sub = &job.settings.video.subtitles;
    let model = crate::subtitles::model_for(sub.model);
    let model_path = crate::subtitles::model_dir()
        .map(|d| d.join(model.file))
        .filter(|p| p.is_file())
        .ok_or_else(|| MediaError::NoModel(model.file.to_owned()))?;
    let language = crate::subtitles::language_arg(&sub.language).ok_or(MediaError::BadLanguage)?;
    job.progress
        .status("msg.verifying", &[model.file.to_owned()]);
    if crate::jar::sha256_of(&model_path)? != model.sha256 {
        return Err(MediaError::ModelMismatch(model.file.to_owned()));
    }
    job.check()?;
    job.progress
        .status("msg.subtitlesTranscribing", &[file_name(input)]);
    let scratch = ScratchDirectory::new(input)?;
    std::os::unix::fs::symlink(&model_path, scratch.path_for("model.bin"))?;
    let text_only = sub.output == SubtitleOutput::Txt;
    let filter = crate::subtitles::filter_arg(
        &language,
        sub.device != SubtitleDevice::Cpu,
        if text_only { "text" } else { "srt" },
    );
    let mut a = s(&media::PREAMBLE);
    a.push("-i".into());
    a.push(media::file_arg(input)?);
    a.extend(s(&["-vn", "-af", &filter, "-f", "null", "-"]));
    media::run_ffmpeg_in(
        &job.env.tools,
        &a,
        info.duration_seconds,
        job.progress,
        Some(scratch.root()),
    )?;
    let produced = scratch.path_for(if text_only { "out.txt" } else { "out.srt" });
    let raw = fs::read_to_string(&produced).unwrap_or_default();
    if raw.trim().is_empty() {
        return Err(MediaError::NoSpeech);
    }
    let duration_ms = (info.duration_seconds.max(0.0) * 1000.0) as u64;
    let (ext, body) = match sub.output {
        SubtitleOutput::Txt => ("txt", raw),
        other => {
            let cues = crate::subtitles::normalize(crate::subtitles::parse_srt(&raw), duration_ms);
            if cues.is_empty() {
                return Err(MediaError::NoSpeech);
            }
            if other == SubtitleOutput::Vtt {
                ("vtt", crate::subtitles::render_vtt(&cues))
            } else {
                ("srt", crate::subtitles::render_srt(&cues))
            }
        }
    };
    let staged = scratch.path_for(&format!("final.{ext}"));
    fs::write(&staged, body)?;
    publish(&staged, input, "", Some(ext))
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub fn audio_encoder(target: &str) -> Vec<String> {
    match target {
        "MP3" => s(&["-c:a", "libmp3lame", "-b:a", "192k"]),
        "M4A" | "AAC" => s(&["-c:a", "aac", "-b:a", "192k"]),
        "OGG" => s(&["-c:a", "libvorbis", "-b:a", "192k"]),
        "WMA" => s(&["-c:a", "wmav2", "-b:a", "192k"]),
        "WAV" => s(&["-c:a", "pcm_s16le"]),
        "AIF" | "AIFF" => s(&["-c:a", "pcm_s16be"]),
        _ => s(&["-c:a", "aac", "-b:a", "192k"]),
    }
}

fn audio_copyable_from(target: &str) -> Option<&'static str> {
    match target {
        "MP3" => Some("mp3"),
        "M4A" | "AAC" => Some("aac"),
        "OGG" => Some("vorbis"),
        _ => None,
    }
}

fn audio_out(
    job: &Job<'_>,
    input: &Path,
    target: &str,
    extract: bool,
) -> Result<PathBuf, MediaError> {
    let info = job.probe(input)?;
    if extract && !info.has_audio() {
        return Err(MediaError::NoAudio);
    }
    let ext = format!(".{}", target.to_lowercase());
    let scratch = ScratchDirectory::new(input)?;
    let staged = scratch.path_for(&format!("out{ext}"));
    if extract
        && audio_copyable_from(target).is_some_and(|c| {
            info.audio_codec
                .as_deref()
                .is_some_and(|a| a.eq_ignore_ascii_case(c))
        })
    {
        match job.ffmpeg(
            &command(input, s(&["-vn", "-c:a", "copy"]), &staged)?,
            info.duration_seconds,
        ) {
            Ok(()) => return publish(&staged, input, "", Some(&ext)),
            Err(MediaError::Ffmpeg { .. }) => safe_delete(&staged),
            Err(e) => return Err(e),
        }
    }
    let mut mid = s(&["-vn"]);
    mid.extend(audio_encoder(target));
    job.ffmpeg(&command(input, mid, &staged)?, info.duration_seconds)?;
    publish(&staged, input, "", Some(&ext))
}

// ------------------------------------------------------------------ compression planning

pub mod plan {
    use super::*;

    pub const MIN_VIDEO_KBPS: i64 = 80;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Container {
        Mp4,
        Mkv,
        WebM,
    }

    pub fn resolve_container(v: &crate::settings::VideoSettings) -> Container {
        match v.container {
            VideoContainer::Auto => {
                if v.codec == VideoCodecChoice::Vp9 {
                    Container::WebM
                } else {
                    Container::Mp4
                }
            }
            VideoContainer::Mp4 => Container::Mp4,
            VideoContainer::Mkv => Container::Mkv,
            VideoContainer::WebM => Container::WebM,
        }
    }

    impl Container {
        pub fn extension(self) -> &'static str {
            match self {
                Self::Mp4 => ".mp4",
                Self::Mkv => ".mkv",
                Self::WebM => ".webm",
            }
        }
        fn setting(self) -> VideoContainer {
            match self {
                Self::Mp4 => VideoContainer::Mp4,
                Self::Mkv => VideoContainer::Mkv,
                Self::WebM => VideoContainer::WebM,
            }
        }
    }

    pub fn can_copy_video(src: &MediaInfo, c: Container) -> bool {
        let codec = src.video_codec.as_deref().unwrap_or("");
        if c == Container::WebM {
            matches!(codec, "vp8" | "vp9" | "av1")
        } else {
            codec.eq_ignore_ascii_case("h264")
        }
    }
    pub fn can_copy_audio(src: &MediaInfo, c: Container) -> bool {
        match src.audio_codec.as_deref() {
            None => false,
            Some(a) if c == Container::WebM => matches!(a, "opus" | "vorbis"),
            Some(a) => a.to_lowercase().starts_with("aac"),
        }
    }
    pub fn already_in(src: &MediaInfo, c: Container) -> bool {
        if c == Container::Mp4 {
            src.is_mp4_family()
        } else {
            src.container.to_lowercase().contains("matroska")
        }
    }

    pub fn safe_target(target: u64) -> u64 {
        let reserve = (64u64 * 1024).max((target as f64 * 0.003).ceil() as u64);
        target.saturating_sub(reserve).max(1)
    }
    pub fn video_kbps(safe_bytes: u64, duration: f64, audio_kbps: i64) -> i64 {
        let total = safe_bytes as f64 * 8.0 / duration / 1000.0;
        let mux = (total * 0.01).max(8.0);
        (total - audio_kbps as f64 - mux).floor() as i64
    }
    pub fn default_audio_kbps(channels: u32) -> i64 {
        match channels {
            0 | 1 => 64,
            2 => 128,
            _ => 160,
        }
    }
    fn bits_per_pixel_frame(kbps: i64, w: u32, h: u32, fps: f64) -> f64 {
        kbps as f64 * 1000.0 / (f64::from(w) * f64::from(h) * fps)
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum AudioMode {
        None,
        Copy,
        Encode,
    }
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct AudioPlan {
        pub mode: AudioMode,
        pub effective_kbps: i64,
        pub encode_kbps: i64,
    }
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Strategy {
        Remux,
        AudioOnly,
        VideoEncode,
    }
    #[derive(Debug, Clone, PartialEq)]
    pub struct Profile {
        pub width: u32,
        pub height: u32,
        pub fps: f64,
        pub video_kbps: i64,
    }
    #[derive(Debug, Clone)]
    pub struct Plan {
        pub safe_target: u64,
        pub strategy: Strategy,
        pub profile: Profile,
        pub audio: AudioPlan,
        pub source: MediaInfo,
        pub container: Container,
    }

    impl Plan {
        pub fn video_copyable(&self) -> bool {
            can_copy_video(&self.source, self.container)
        }
        pub fn max_full_attempts(&self) -> u32 {
            2
        }
    }

    #[derive(Clone, Copy)]
    struct Cand {
        w: u32,
        h: u32,
        fps: f64,
    }

    fn scaled_to_short_side(w: u32, h: u32, short: u32, fps: f64) -> Cand {
        if w >= h {
            let sh = media::make_even(short.min(h));
            Cand {
                w: media::make_even((f64::from(sh) * f64::from(w) / f64::from(h)).round() as u32),
                h: sh,
                fps,
            }
        } else {
            let sw = media::make_even(short.min(w));
            Cand {
                w: sw,
                h: media::make_even((f64::from(sw) * f64::from(h) / f64::from(w)).round() as u32),
                fps,
            }
        }
    }

    pub fn build_profile(src: &MediaInfo, kbps: i64) -> Profile {
        let (w, h) = (media::make_even(src.width), media::make_even(src.height));
        let fps = if src.frame_rate > 0.0 {
            src.frame_rate
        } else {
            30.0
        };
        let clamp = |v: f64| v.min(fps);
        let raw = [
            Cand { w, h, fps },
            Cand {
                w,
                h,
                fps: clamp(30.0),
            },
            Cand {
                w,
                h,
                fps: clamp(24.0),
            },
            scaled_to_short_side(w, h, 1080, clamp(24.0)),
            scaled_to_short_side(w, h, 720, clamp(24.0)),
            scaled_to_short_side(w, h, 480, clamp(24.0)),
        ];
        let mut seen = std::collections::HashSet::new();
        let cands: Vec<Cand> = raw
            .into_iter()
            .filter(|c| c.w <= w && c.h <= h && c.fps <= fps + 0.001)
            .filter(|c| seen.insert(format!("{}x{}@{:.3}", c.w, c.h, c.fps)))
            .collect();
        let pick = cands
            .iter()
            .find(|c| bits_per_pixel_frame(kbps, c.w, c.h, c.fps) >= 0.03)
            .or(cands.last())
            .copied()
            .unwrap_or(Cand { w, h, fps });
        Profile {
            width: pick.w,
            height: pick.h,
            fps: pick.fps,
            video_kbps: kbps,
        }
    }

    fn build_audio(src: &MediaInfo, c: Container) -> AudioPlan {
        if src.audio_tracks == 0 {
            return AudioPlan {
                mode: AudioMode::None,
                effective_kbps: 0,
                encode_kbps: 0,
            };
        }
        let requested = default_audio_kbps(src.audio_channels);
        let source_kbps =
            (src.audio_bitrate > 0).then(|| (src.audio_bitrate as f64 / 1000.0).round() as i64);
        match source_kbps {
            Some(k) if can_copy_audio(src, c) && k <= requested => AudioPlan {
                mode: AudioMode::Copy,
                effective_kbps: k,
                encode_kbps: requested,
            },
            _ => AudioPlan {
                mode: AudioMode::Encode,
                effective_kbps: requested,
                encode_kbps: requested,
            },
        }
    }

    pub fn build(src: &MediaInfo, target_bytes: u64, container: Container) -> Plan {
        let safe = safe_target(target_bytes);
        let audio = build_audio(src, container);
        let aim = (safe as f64 * 0.93) as u64;
        let mut kbps = video_kbps(aim, src.duration_seconds.max(0.1), audio.effective_kbps);
        if src.video_bitrate > 0 {
            let source_kbps = (src.video_bitrate / 1000) as i64;
            if kbps > source_kbps {
                kbps = MIN_VIDEO_KBPS.max((source_kbps as f64 * 0.98) as i64);
            }
        }
        kbps = kbps.max(MIN_VIDEO_KBPS);

        let video_copyable = can_copy_video(src, container);
        let removable = src.audio_tracks > 1 || src.subtitle_tracks > 0;
        let remuxable = video_copyable
            && (src.audio_tracks == 0 || can_copy_audio(src, container))
            && (!already_in(src, container) || removable);
        let strategy = if remuxable {
            Strategy::Remux
        } else if video_copyable && audio.mode == AudioMode::Encode {
            Strategy::AudioOnly
        } else {
            Strategy::VideoEncode
        };
        Plan {
            safe_target: safe,
            strategy,
            profile: build_profile(src, kbps),
            audio,
            source: src.clone(),
            container,
        }
    }

    pub fn container_flags(c: Container) -> Vec<String> {
        if c == Container::Mp4 {
            s(&["-movflags", "+faststart"])
        } else {
            vec![]
        }
    }

    fn track_maps() -> Vec<String> {
        s(&["-map", "0:v:0", "-map", "0:a:0?"])
    }

    fn audio_args(p: &Plan) -> Vec<String> {
        match p.audio.mode {
            AudioMode::None => s(&["-an"]),
            AudioMode::Copy => s(&["-c:a", "copy"]),
            AudioMode::Encode => s(&[
                "-c:a",
                if p.container == Container::WebM {
                    "libopus"
                } else {
                    "aac"
                },
                "-b:a",
                &format!("{}k", p.audio.encode_kbps),
            ]),
        }
    }

    pub fn video_filter(p: &Plan) -> Vec<String> {
        let (sw, sh) = (
            media::make_even(p.source.width),
            media::make_even(p.source.height),
        );
        let sfps = if p.source.frame_rate > 0.0 {
            p.source.frame_rate
        } else {
            30.0
        };
        let mut parts = Vec::new();
        if p.profile.width != sw || p.profile.height != sh {
            parts.push(format!("scale={}:{}", p.profile.width, p.profile.height));
            parts.push("setsar=1".to_owned());
        }
        if (p.profile.fps - sfps).abs() > 0.01 {
            parts.push(format!("fps={}", (p.profile.fps * 1000.0).round() / 1000.0));
        }
        if parts.is_empty() {
            vec![]
        } else {
            vec!["-vf".into(), parts.join(",")]
        }
    }

    pub fn remux_args(p: &Plan) -> Vec<String> {
        let mut a = track_maps();
        a.extend(s(&["-c", "copy"]));
        a.extend(container_flags(p.container));
        a
    }
    pub fn audio_only_args(p: &Plan) -> Vec<String> {
        let mut a = track_maps();
        a.extend(s(&["-c:v", "copy"]));
        a.extend(audio_args(p));
        a.extend(container_flags(p.container));
        a
    }
    pub fn single_pass_args(
        p: &Plan,
        enc: &EncoderChoice,
        kbps: i64,
        threads: Option<u32>,
        priority: crate::settings::EncodePriority,
    ) -> Vec<String> {
        let mut a = track_maps();
        a.extend(video_filter(p));
        a.extend(threads_args(threads));
        a.extend(media::bitrate_args(enc, kbps.max(10) as u32, priority));
        a.extend(media::pixel_format_args(enc));
        a.extend(media::tag_args(enc, p.container.extension()));
        a.extend(audio_args(p));
        a.extend(container_flags(p.container));
        a
    }

    pub(super) fn container_setting(c: Container) -> VideoContainer {
        c.setting()
    }
}

struct Outcome {
    path: PathBuf,
    bytes: u64,
    reached: bool,
}

fn try_stage(
    job: &Job<'_>,
    p: &plan::Plan,
    input: &Path,
    staged: &Path,
    args: Vec<String>,
    label: &str,
    duration: f64,
) -> Result<Option<u64>, MediaError> {
    job.progress
        .status("msg.compressStage", &[label.to_owned()]);
    safe_delete(staged);
    match job.ffmpeg(&command(input, args, staged)?, duration) {
        Ok(()) => {}
        Err(MediaError::Cancelled) => return Err(MediaError::Cancelled),
        Err(_) => {
            safe_delete(staged);
            return Ok(None);
        }
    }
    let Ok(meta) = fs::metadata(staged) else {
        return Ok(None);
    };
    if meta.len() <= p.safe_target {
        return Ok(Some(meta.len()));
    }
    safe_delete(staged);
    Ok(None)
}

fn compress_pipeline(
    job: &Job<'_>,
    p: &plan::Plan,
    input: &Path,
    scratch: &ScratchDirectory,
) -> Result<Outcome, MediaError> {
    let duration = p.source.duration_seconds;
    let ext = p.container.extension();
    if p.strategy == plan::Strategy::Remux {
        let out = scratch.path_for(&format!("remux{ext}"));
        if let Some(bytes) = try_stage(job, p, input, &out, plan::remux_args(p), "remux", duration)?
        {
            return Ok(Outcome {
                path: out,
                bytes,
                reached: true,
            });
        }
    }
    if p.video_copyable() && p.audio.mode == plan::AudioMode::Encode {
        let out = scratch.path_for(&format!("audio{ext}"));
        if let Some(bytes) = try_stage(
            job,
            p,
            input,
            &out,
            plan::audio_only_args(p),
            "audio",
            duration,
        )? {
            return Ok(Outcome {
                path: out,
                bytes,
                reached: true,
            });
        }
    }
    let priority = job.settings.video.priority;
    let mut kbps = p.profile.video_kbps;
    let mut best: Option<(PathBuf, u64)> = None;
    for attempt in 1..=p.max_full_attempts() {
        job.check()?;
        let out = scratch.path_for(&format!("encode-{attempt}{ext}"));
        job.progress.status(
            "msg.compressEncode",
            &[
                format!("{}x{}", p.profile.width, p.profile.height),
                format!("{:.0}", p.profile.fps),
                kbps.to_string(),
            ],
        );
        run_encode(job, &out, ext, duration, &|enc, t| {
            command(
                input,
                plan::single_pass_args(p, enc, kbps, t, priority),
                &out,
            )
        })?;
        let size = fs::metadata(&out)?.len();
        if size <= p.safe_target {
            return Ok(Outcome {
                path: out,
                bytes: size,
                reached: true,
            });
        }
        if best.as_ref().is_none_or(|(_, b)| size < *b) {
            if let Some((old, _)) = best.take() {
                safe_delete(&old);
            }
            best = Some((out, size));
        } else {
            safe_delete(&out);
        }
        if attempt < p.max_full_attempts() {
            kbps = 10.max((kbps as f64 * (p.safe_target as f64 / size as f64) * 0.97) as i64);
        }
    }
    best.map(|(path, bytes)| Outcome {
        path,
        bytes,
        reached: false,
    })
    .ok_or_else(|| MediaError::Ffmpeg {
        code: Some(1),
        tail: "every compression attempt failed".into(),
    })
}

fn compress(
    job: &Job<'_>,
    input: &Path,
    params: &HashMap<String, String>,
) -> Result<Done, MediaError> {
    let video = &job.settings.video;
    let target = if params.get("preset").is_some_and(|v| v == "discord") {
        video.presets.compress_discord.max(1) as u64
    } else {
        params
            .get("bytes")
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v > 0)
            .ok_or_else(|| MediaError::Unsupported("bytes".into()))?
    };
    let name = input
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let original = fs::metadata(input)?.len();
    if original <= plan::safe_target(target) {
        job.info("msg.alreadySmall", vec![name, original.to_string()]);
        return Ok(finish(job, input.to_path_buf()));
    }
    let info = job.probe(input)?;
    if info.duration_seconds <= 0.0 {
        return Err(MediaError::NoDuration);
    }
    let container = plan::resolve_container(video);
    let _ = plan::container_setting(container);
    let p = plan::build(&info, target, container);
    let scratch = ScratchDirectory::new(input)?;
    let outcome = if info.has_video() {
        compress_pipeline(job, &p, input, &scratch)?
    } else {
        // audio-only source: one AAC pass sized to the budget
        let budget_kbps =
            (p.safe_target as f64 * 8.0 / info.duration_seconds.max(0.1) / 1000.0) as i64;
        let kbps = (budget_kbps - 4).clamp(8, 320);
        let out = scratch.path_for("audio-source.m4a");
        job.ffmpeg(
            &command(
                input,
                s(&[
                    "-vn",
                    "-c:a",
                    "aac",
                    "-b:a",
                    &format!("{kbps}k"),
                    "-movflags",
                    "+faststart",
                ]),
                &out,
            )?,
            info.duration_seconds,
        )?;
        let bytes = fs::metadata(&out)?.len();
        Outcome {
            path: out,
            bytes,
            reached: bytes <= p.safe_target,
        }
    };
    let ext = if info.has_video() {
        p.container.extension()
    } else {
        ".m4a"
    };
    let output = publish(&outcome.path, input, "_compressed", Some(ext))?;
    if !outcome.reached || outcome.bytes > target {
        job.warn(
            "msg.overTarget",
            vec![name, outcome.bytes.to_string(), target.to_string()],
        );
    }
    Ok(finish(job, output))
}

fn finish(job: &Job<'_>, output: PathBuf) -> Done {
    Done {
        output,
        warnings: job.warnings.take(),
        infos: job.infos.take(),
    }
}

/// Entry point for all media tasks.
pub fn run(
    name: &str,
    params: &HashMap<String, String>,
    input: &Path,
    job: &Job<'_>,
) -> Result<Done, MediaError> {
    job.check()?;
    let video = &job.settings.video;
    let output = match name {
        "video.resize" => filter_task(job, input, "_resized", &scale_filter(params))?,
        "video.rotate" => filter_task(
            job,
            input,
            "_rotated",
            &rotate_filter(
                param_f64(params, "deg").unwrap_or(90.0),
                &video.rotate_fill_color,
            ),
        )?,
        "video.convert" => convert(
            job,
            input,
            &params
                .get("to")
                .map(|t| t.to_uppercase())
                .unwrap_or_else(|| "MP4".into()),
        )?,
        "video.trim" => trim(job, input, params)?,
        "video.thumbnail" => thumbnail(job, input, params)?,
        "video.extractAudio" => audio_out(
            job,
            input,
            &params
                .get("to")
                .map(|t| t.to_uppercase())
                .unwrap_or_else(|| "MP3".into()),
            true,
        )?,
        "audio.convert" => audio_out(
            job,
            input,
            &params
                .get("to")
                .map(|t| t.to_uppercase())
                .unwrap_or_else(|| "MP3".into()),
            false,
        )?,
        "video.subtitles" | "audio.subtitles" => subtitles(job, input)?,
        "video.compress" => return compress(job, input, params),
        _ => return Err(MediaError::Unsupported(name.to_owned())),
    };
    Ok(finish(job, output))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::folder::NullProgress;
    use crate::settings::EncodePriority;

    fn params(q: &str) -> HashMap<String, String> {
        crate::tasks::parse_task(&format!("x?{q}")).1
    }

    fn info(
        w: u32,
        h: u32,
        fps: f64,
        vcodec: &str,
        acodec: Option<&str>,
        container: &str,
        kbps: u64,
    ) -> MediaInfo {
        MediaInfo {
            duration_seconds: 600.0,
            width: w,
            height: h,
            frame_rate: fps,
            video_codec: Some(vcodec.into()),
            audio_codec: acodec.map(str::to_owned),
            video_bitrate: kbps * 1000,
            audio_bitrate: 128_000,
            audio_channels: 2,
            audio_tracks: u32::from(acodec.is_some()),
            subtitle_tracks: 0,
            container: container.into(),
        }
    }

    #[test]
    fn scale_filters() {
        assert_eq!(
            scale_filter(&params("scale=50")),
            "scale=trunc(iw*0.5/2)*2:trunc(ih*0.5/2)*2"
        );
        assert_eq!(
            scale_filter(&params("scale=12.5")),
            "scale=trunc(iw*0.125/2)*2:trunc(ih*0.125/2)*2"
        );
        assert_eq!(
            scale_filter(&params("scale=-5")),
            "scale=trunc(iw*0.5/2)*2:trunc(ih*0.5/2)*2",
            "nonsense falls back to 50%"
        );
        assert_eq!(
            scale_filter(&params("scale=custom&w=801&h=0")),
            "scale=800:-2"
        );
        assert_eq!(
            scale_filter(&params("scale=custom&w=0&h=481")),
            "scale=-2:480"
        );
        assert_eq!(
            scale_filter(&params("scale=custom&w=640&h=360&keep=1")),
            format!("scale=640:360:force_original_aspect_ratio=decrease,{EVEN}")
        );
        assert_eq!(
            scale_filter(&params("scale=custom&w=640&h=361&keep=0")),
            "scale=640:360"
        );
        assert_eq!(
            scale_filter(&params("scale=custom&w=1e99&h=0")),
            "scale=16384:-2",
            "huge values are clamped"
        );
    }

    #[test]
    fn rotate_filters_never_carry_user_text() {
        assert_eq!(rotate_filter(90.0, "#000"), format!("transpose=1,{EVEN}"));
        assert_eq!(rotate_filter(-90.0, "#000"), format!("transpose=2,{EVEN}"));
        assert_eq!(
            rotate_filter(180.0, "#000"),
            format!("transpose=1,transpose=1,{EVEN}")
        );
        assert!(rotate_filter(45.0, "#FF8000").contains("fillcolor=0xFF8000"));
        assert!(
            rotate_filter(45.0, "'; drop").contains("fillcolor=0x000000"),
            "a bad colour cannot reach the filter graph"
        );
        assert!(rotate_filter(1e300, "#000").contains("3600*PI/180"));
    }

    #[test]
    fn bitrate_math_matches_the_windows_planner() {
        assert_eq!(
            plan::safe_target(10 * 1024 * 1024),
            10 * 1024 * 1024 - 64 * 1024,
            "small targets reserve 64 KiB"
        );
        // 0.3% reserve once it exceeds 64 KiB
        let t = 100u64 * 1024 * 1024;
        assert_eq!(plan::safe_target(t), t - (t as f64 * 0.003).ceil() as u64);
        assert_eq!(plan::safe_target(1), 1);
        assert_eq!(plan::default_audio_kbps(1), 64);
        assert_eq!(plan::default_audio_kbps(2), 128);
        assert_eq!(plan::default_audio_kbps(6), 160);
        // 20 MiB over 600 s with 128k audio
        let kbps = plan::video_kbps(plan::safe_target(20 << 20), 600.0, 128);
        assert_eq!(
            kbps, 142,
            "20 MiB -> 278 kbps in total, minus 128 audio and 8 muxing reserve"
        );
    }

    #[test]
    fn profile_drops_fps_then_resolution_until_quality_is_plausible() {
        let src = info(1920, 1080, 60.0, "h264", Some("aac"), "mov,mp4", 20_000);
        assert_eq!(
            plan::build_profile(&src, 20_000),
            plan::Profile {
                width: 1920,
                height: 1080,
                fps: 60.0,
                video_kbps: 20_000
            }
        );
        let mid = plan::build_profile(&src, 3000);
        assert_eq!((mid.width, mid.height), (1920, 1080));
        assert!(mid.fps <= 30.0);
        let low = plan::build_profile(&src, 300);
        assert!(low.height <= 720 && low.fps <= 24.0, "{low:?}");
        let floor = plan::build_profile(&src, 80);
        assert_eq!(floor.height, 480, "never below the smallest candidate");
        assert_eq!(floor.width % 2 + floor.height % 2, 0);
        // portrait video scales by its short side
        let tall = info(1080, 1920, 30.0, "h264", None, "mov,mp4", 8000);
        let p = plan::build_profile(&tall, 150);
        assert!(p.width <= 480 && p.height > p.width, "{p:?}");
        // an odd-sized source is evened
        assert_eq!(
            plan::build_profile(&info(641, 481, 30.0, "h264", None, "x", 0), 99_999).width,
            640
        );
    }

    #[test]
    fn strategy_follows_what_can_be_copied() {
        let mp4 = plan::Container::Mp4;
        let s = |i: &MediaInfo| plan::build(i, 5 << 20, mp4).strategy;
        // already h264+aac in mp4: nothing to repackage, audio fine -> must re-encode video
        assert_eq!(
            s(&info(1280, 720, 30.0, "h264", Some("aac"), "mov,mp4", 8000)),
            plan::Strategy::VideoEncode
        );
        // h264+aac in mkv: repackaging into mp4 alone might already fit
        assert_eq!(
            s(&info(
                1280,
                720,
                30.0,
                "h264",
                Some("aac"),
                "matroska,webm",
                8000
            )),
            plan::Strategy::Remux
        );
        // h264 with ac3 audio: copy video, encode audio
        assert_eq!(
            s(&info(
                1280,
                720,
                30.0,
                "h264",
                Some("ac3"),
                "matroska,webm",
                8000
            )),
            plan::Strategy::AudioOnly
        );
        // hevc: full encode
        assert_eq!(
            s(&info(1280, 720, 30.0, "hevc", Some("aac"), "mov,mp4", 8000)),
            plan::Strategy::VideoEncode
        );
        // extra audio tracks are a reason to remux even inside mp4
        let mut extra = info(1280, 720, 30.0, "h264", Some("aac"), "mov,mp4", 8000);
        extra.audio_tracks = 2;
        assert_eq!(s(&extra), plan::Strategy::Remux);
        // webm
        assert!(plan::can_copy_video(
            &info(1, 1, 1.0, "vp9", None, "", 0),
            plan::Container::WebM
        ));
        assert!(!plan::can_copy_audio(
            &info(1, 1, 1.0, "vp9", Some("aac"), "", 0),
            plan::Container::WebM
        ));
        // no audio at all: AudioMode::None
        assert_eq!(
            plan::build(&info(640, 360, 30.0, "h264", None, "x", 1000), 1 << 20, mp4)
                .audio
                .mode,
            plan::AudioMode::None
        );
    }

    #[test]
    fn the_video_bitrate_never_exceeds_the_source_and_never_drops_below_the_floor() {
        let low = info(640, 360, 30.0, "hevc", Some("aac"), "mov,mp4", 500);
        assert_eq!(
            plan::build(&low, 1 << 30, plan::Container::Mp4)
                .profile
                .video_kbps,
            490,
            "98% of the source bitrate"
        );
        let long = MediaInfo {
            duration_seconds: 36_000.0,
            ..low
        };
        assert_eq!(
            plan::build(&long, 1 << 20, plan::Container::Mp4)
                .profile
                .video_kbps,
            plan::MIN_VIDEO_KBPS
        );
    }

    #[test]
    fn single_pass_arguments() {
        let src = info(1920, 1080, 60.0, "hevc", Some("ac3"), "mov,mp4", 20_000);
        let p = plan::build(&src, 20 << 20, plan::Container::Mp4);
        let enc = EncoderChoice {
            name: "libx264".into(),
            hardware: false,
        };
        let a = plan::single_pass_args(&p, &enc, 900, Some(2), EncodePriority::Balanced);
        let joined = a.join(" ");
        assert!(joined.starts_with("-map 0:v:0 -map 0:a:0?"), "{joined}");
        assert!(
            joined.contains("-vf scale=")
                && joined.contains("-threads 2")
                && joined.contains("-b:v 900k")
                && joined.contains("-c:a aac")
                && joined.contains("-movflags +faststart"),
            "{joined}"
        );
        let webm = plan::build(&src, 20 << 20, plan::Container::WebM);
        assert!(
            plan::single_pass_args(&webm, &enc, 900, None, EncodePriority::Speed)
                .join(" ")
                .contains("-c:a libopus")
        );
        assert!(
            !plan::single_pass_args(&webm, &enc, 900, None, EncodePriority::Speed)
                .join(" ")
                .contains("faststart")
        );
        assert_eq!(
            plan::remux_args(&p).join(" "),
            "-map 0:v:0 -map 0:a:0? -c copy -movflags +faststart"
        );
    }

    // ----------------------------------------------------------- end to end with a real ffmpeg

    fn env() -> Option<MediaEnv> {
        let tools = Tools::from_dir(Path::new("/usr/bin"))
            .or_else(|| Tools::from_dir(Path::new("/usr/local/bin")))?;
        let caps = Capabilities::detect(&tools, false);
        caps.encoders
            .contains("libx264")
            .then_some(MediaEnv { tools, caps })
    }

    macro_rules! need_env {
        () => {
            match env() {
                Some(e) => e,
                None => {
                    eprintln!("SKIPPED: no ffmpeg with libx264 in /usr/bin");
                    return;
                }
            }
        };
    }

    fn make_video(env: &MediaEnv, path: &Path, secs: u32, size: &str) {
        let st = std::process::Command::new(&env.tools.ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-f",
                "lavfi",
                "-i",
            ])
            .arg(format!("testsrc2=duration={secs}:size={size}:rate=25"))
            .args(["-f", "lavfi", "-i"])
            .arg(format!("sine=frequency=440:duration={secs}"))
            .args([
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-g",
                "25",
                "-c:a",
                "aac",
                "-shortest",
            ])
            .arg(path)
            .status()
            .unwrap();
        assert!(st.success());
    }

    fn job<'a>(env: &'a MediaEnv, s: &'a Settings) -> Job<'a> {
        Job::new(env, s, &NullProgress)
    }

    fn dims(env: &MediaEnv, p: &Path) -> (u32, u32) {
        let i = media::probe(&env.tools, p, &|| false).unwrap();
        (i.width, i.height)
    }

    #[test]
    fn e2e_resize_rotate_and_copy_audio() {
        let env = need_env!();
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("clip.mp4");
        make_video(&env, &src, 2, "320x240");
        let s = Settings::default();
        let r = run("video.resize", &params("scale=50"), &src, &job(&env, &s)).unwrap();
        assert_eq!(
            (r.output.clone(), dims(&env, &r.output)),
            (d.path().join("clip_resized.mp4"), (160, 120))
        );
        let r = run(
            "video.resize",
            &params("scale=custom&w=100&h=0"),
            &src,
            &job(&env, &s),
        )
        .unwrap();
        assert_eq!(dims(&env, &r.output), (100, 76));
        let r = run("video.rotate", &params("deg=90"), &src, &job(&env, &s)).unwrap();
        assert_eq!(dims(&env, &r.output), (240, 320));
        let r = run("video.rotate", &params("deg=45"), &src, &job(&env, &s)).unwrap();
        let (w, h) = dims(&env, &r.output);
        assert!(w % 2 == 0 && h % 2 == 0 && w > 320);
        assert_eq!(
            media::probe(&env.tools, &r.output, &|| false)
                .unwrap()
                .audio_codec
                .as_deref(),
            Some("aac")
        );
        assert_eq!(
            fs::read_dir(d.path())
                .unwrap()
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with(".Flect"))
                .count(),
            0,
            "no scratch litter"
        );
    }

    #[test]
    fn e2e_convert_trim_thumbnail_audio() {
        let env = need_env!();
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("clip.mp4");
        make_video(&env, &src, 4, "320x240");
        let s = Settings::default();

        let mkv = run("video.convert", &params("to=MKV"), &src, &job(&env, &s)).unwrap();
        assert_eq!(mkv.output, d.path().join("clip.mkv"));
        assert!(media::probe(&env.tools, &mkv.output, &|| false)
            .unwrap()
            .container
            .contains("matroska"));

        let gif = run("video.convert", &params("to=GIF"), &src, &job(&env, &s)).unwrap();
        assert_eq!(gif.output, d.path().join("clip.gif"));
        let gif_bytes = fs::read(&gif.output).unwrap();
        assert!(gif_bytes.starts_with(b"GIF89a") || gif_bytes.starts_with(b"GIF87a"));
        assert!(crate::imaging::load(&gif.output).unwrap().is_animated());

        let t = run("video.trim", &params("start=1&end=3"), &src, &job(&env, &s)).unwrap();
        let ti = media::probe(&env.tools, &t.output, &|| false).unwrap();
        assert!(
            (1.5..=3.2).contains(&ti.duration_seconds),
            "keyframe trim around 2 s, got {}",
            ti.duration_seconds
        );
        let mut exact = Settings::default();
        exact.video.trim.accuracy = TrimAccuracy::Exact;
        let t = run(
            "video.trim",
            &params("start=1&end=3"),
            &src,
            &job(&env, &exact),
        )
        .unwrap();
        assert!(
            (media::probe(&env.tools, &t.output, &|| false)
                .unwrap()
                .duration_seconds
                - 2.0)
                .abs()
                < 0.2
        );
        assert_eq!(
            run("video.trim", &params("start=3&end=1"), &src, &job(&env, &s))
                .unwrap_err()
                .key(),
            "dialog.trim.backwards"
        );

        let th = run("video.thumbnail", &params("t=1.5"), &src, &job(&env, &s)).unwrap();
        assert_eq!(th.output, d.path().join("clip_thumbnail.png"));
        assert_eq!(
            crate::imaging::load(&th.output)
                .unwrap()
                .still()
                .dimensions(),
            (320, 240)
        );

        let wav = run(
            "video.extractAudio",
            &params("to=WAV"),
            &src,
            &job(&env, &s),
        )
        .unwrap();
        assert_eq!(wav.output, d.path().join("clip.wav"));
        let wi = media::probe(&env.tools, &wav.output, &|| false).unwrap();
        assert!(!wi.has_video() && (wi.duration_seconds - 4.0).abs() < 0.3);
        let aac = run(
            "audio.convert",
            &params("to=M4A"),
            &wav.output,
            &job(&env, &s),
        )
        .unwrap();
        assert_eq!(aac.output, d.path().join("clip.m4a"));

        let silent = d.path().join("silent.mp4");
        std::process::Command::new(&env.tools.ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-f",
                "lavfi",
                "-i",
                "color=c=red:s=64x64:d=1",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&silent)
            .status()
            .unwrap();
        assert_eq!(
            run(
                "video.extractAudio",
                &params("to=MP3"),
                &silent,
                &job(&env, &s)
            )
            .unwrap_err()
            .key(),
            "msg.noAudioTrack"
        );
    }

    #[test]
    fn e2e_compress_to_a_target() {
        let env = need_env!();
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("big.mp4");
        make_video(&env, &src, 20, "640x480");
        let original = fs::metadata(&src).unwrap().len();
        let s = Settings::default();
        let target = original / 3;
        let r = run(
            "video.compress",
            &params(&format!("bytes={target}")),
            &src,
            &job(&env, &s),
        )
        .unwrap();
        let got = fs::metadata(&r.output).unwrap().len();
        assert_eq!(r.output, d.path().join("big_compressed.mp4"));
        assert!(
            got <= target && r.warnings.is_empty(),
            "{got} > {target}: {:?}",
            r.warnings
        );
        assert!(media::probe(&env.tools, &r.output, &|| false)
            .unwrap()
            .has_audio());
        // already below the target: nothing is written
        let small = run(
            "video.compress",
            &params(&format!("bytes={}", original * 2)),
            &src,
            &job(&env, &s),
        )
        .unwrap();
        assert_eq!(small.infos[0].0, "msg.alreadySmall");
        assert!(!d.path().join("big_compressed (2).mp4").exists());
        // impossible target: still produces the best effort and says so
        let tiny = run(
            "video.compress",
            &params("bytes=20000"),
            &src,
            &job(&env, &s),
        )
        .unwrap();
        assert_eq!(tiny.warnings[0].0, "msg.overTarget");
        assert!(tiny.output.exists());
        assert!(run("video.compress", &HashMap::new(), &src, &job(&env, &s)).is_err());
    }

    #[test]
    fn e2e_hostile_and_cancelled() {
        let env = need_env!();
        let d = tempfile::tempdir().unwrap();
        let s = Settings::default();
        // not media at all
        let junk = d.path().join("junk.mp4");
        fs::write(&junk, b"this is not a video").unwrap();
        assert_eq!(
            run("video.resize", &params("scale=50"), &junk, &job(&env, &s))
                .unwrap_err()
                .key(),
            "msg.mediaProbeFailed"
        );
        // an HLS playlist pointing at a local file must not be followed
        let secret = d.path().join("secret.txt");
        fs::write(&secret, "TOP SECRET").unwrap();
        let m3u8 = d.path().join("evil.mp4");
        fs::write(
            &m3u8,
            format!(
                "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\n{}\n#EXT-X-ENDLIST\n",
                secret.display()
            ),
        )
        .unwrap();
        let err = run("video.resize", &params("scale=50"), &m3u8, &job(&env, &s)).unwrap_err();
        assert!(
            matches!(err.key(), "msg.mediaUnsupported" | "msg.mediaProbeFailed"),
            "{err:?}"
        );
        // an audio-only file has no video stream to resize
        let src = d.path().join("clip.mp4");
        make_video(&env, &src, 1, "64x64");
        let wav = run(
            "video.extractAudio",
            &params("to=WAV"),
            &src,
            &job(&env, &s),
        )
        .unwrap()
        .output;
        assert_eq!(
            run("video.resize", &params("scale=50"), &wav, &job(&env, &s))
                .unwrap_err()
                .key(),
            "msg.noVideoStream"
        );
        // cancel before start
        struct Stop;
        impl Progress for Stop {
            fn cancelled(&self) -> bool {
                true
            }
        }
        let stop = Stop;
        let j = Job::new(&env, &s, &stop);
        assert!(matches!(
            run("video.resize", &params("scale=50"), &src, &j),
            Err(MediaError::Cancelled)
        ));
        assert!(!d.path().join("clip_resized.mp4").exists());
    }

    #[test]
    fn e2e_cancel_kills_ffmpeg_mid_encode() {
        let env = need_env!();
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("long.mp4");
        make_video(&env, &src, 120, "1280x720");
        let s = Settings::default();
        struct After(std::time::Instant);
        impl Progress for After {
            fn cancelled(&self) -> bool {
                self.0.elapsed() > std::time::Duration::from_millis(600)
            }
        }
        let p = After(std::time::Instant::now());
        let j = Job::new(&env, &s, &p);
        let t = std::time::Instant::now();
        let r = run("video.resize", &params("scale=custom&w=1920&h=0"), &src, &j);
        assert!(matches!(r, Err(MediaError::Cancelled)), "{r:?}");
        assert!(
            t.elapsed() < std::time::Duration::from_secs(8),
            "took {:?}",
            t.elapsed()
        );
        let left: Vec<_> = fs::read_dir(d.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, ["long.mp4"], "cancelled run cleaned up: {left:?}");
    }

    #[test]
    fn e2e_subtitles_need_whisper_and_a_pinned_model() {
        let env = need_env!();
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("clip.mp4");
        make_video(&env, &src, 2, "160x120");
        let s = Settings::default();
        match run("video.subtitles", &params(""), &src, &job(&env, &s)) {
            // Only a machine with a whisper-enabled ffmpeg and an installed model gets this far.
            Ok(_) | Err(MediaError::NoSpeech) => {}
            Err(MediaError::NoWhisper) | Err(MediaError::NoModel(_)) => {}
            Err(e) => panic!("unexpected: {e:?}"),
        }
        let left: Vec<_> = fs::read_dir(d.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != "clip.mp4" && !n.ends_with(".srt"))
            .collect();
        assert!(left.is_empty(), "scratch left behind: {left:?}");
    }
}
