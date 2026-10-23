//! `settings.json`, schema-compatible with the Windows app (PascalCase keys,
//! enum names as strings) so an old file can be imported as-is.
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::workspace::unique_token;

macro_rules! choice {
    ($name:ident { $($variant:ident),+ $(,)? } default $default:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        pub enum $name { $($variant),+ }
        impl Default for $name { fn default() -> Self { Self::$default } }
    };
}

choice!(AppTheme { System, Light, Dark } default System);
choice!(AppLanguage { System, English, Japanese, ChineseSimplified } default System);
choice!(DuplicateStrictness { SizeOnly, HeadHash, FullContent } default FullContent);
choice!(DuplicateKeep { Ask, Oldest, Newest } default Ask);
choice!(DeleteMethod { RecycleBin, Permanent } default RecycleBin);
choice!(VideoCodecChoice { H264, H265, Vp9, Av1 } default H264);
choice!(VideoContainer { Auto, Mp4, Mkv, WebM } default Auto);
// Nvenc/Qsv/Amf are kept so a Windows file still loads; they are never offered on macOS.
choice!(HardwareEncoder { None, Nvenc, Qsv, Amf, VideoToolbox } default None);
choice!(EncodePriority { Speed, Balanced, Quality } default Balanced);
choice!(GifDither { None, Bayer, FloydSteinberg } default Bayer);
choice!(TrimAccuracy { Keyframe, Exact } default Keyframe);
choice!(ThumbnailFormat { Png, Jpg } default Png);
choice!(SubtitleModel { Tiny, Base, Small, Medium, LargeV3, Turbo } default Small);
choice!(SubtitleDevice { Auto, Cpu } default Auto);
choice!(SubtitleOutput { Srt, Vtt, Txt } default Srt);
choice!(ArchiveCompressFormat { Zip, SevenZip, Tar, TarGz } default Zip);
choice!(BackgroundRemovalMode { AiWithFallback, AiOnly, ChromaKeyOnly } default AiWithFallback);
choice!(SjisFallback { ReplaceAndWarn, Substitute, Fail } default ReplaceAndWarn);
choice!(JsonKeyOrder { Ordinal, IgnoreCase, Natural } default Ordinal);
choice!(OcrOutput { TextFile, Clipboard, ShowOnly } default TextFile);
choice!(TextEncodingChoice { Utf8, Utf8Bom, Utf16, ShiftJis } default Utf8Bom);
choice!(ReportFormat { Csv, Json, Html, Txt } default Csv);

const MIB: i64 = 1024 * 1024;
pub const DISCORD_LIMIT: i64 = 20 * MIB;
pub const LEGACY_DISCORD_LIMIT: i64 = 10 * MIB;

macro_rules! section {
    ($(#[$m:meta])* $name:ident { $($field:ident : $ty:ty = $default:expr),* $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        #[serde(rename_all = "PascalCase", default)]
        pub struct $name { $(pub $field: $ty),* }
        impl Default for $name {
            fn default() -> Self { Self { $($field: $default),* } }
        }
    };
}

section!(ExclusionRules {
    folders: String = String::new(),
    files: String = String::new(),
    extensions: String = String::new()
});
section!(GeneralSettings {
    theme: AppTheme = AppTheme::System,
    language: AppLanguage = AppLanguage::System,
    close_on_success: bool = true,
    close_on_failure: bool = false,
    close_on_cancel: bool = true,
    auto_update: bool = true,
    hidden_menu_items: Vec<String> = vec![],
});
section!(DuplicateSettings {
    strictness: DuplicateStrictness = DuplicateStrictness::FullContent,
    match_name: bool = false,
    match_timestamp: bool = false,
    ignore_empty_files: bool = true,
    minimum_bytes: i64 = 0,
    keep: DuplicateKeep = DuplicateKeep::Ask,
});
section!(RenameSettings {
    use_numbering: bool = true,
    digit_count: i32 = 0,
    start_number: i32 = 1,
    dry_run: bool = false,
    exclusions: ExclusionRules = ExclusionRules::default(),
});
// IgnoreDesktopIni / IgnoreThumbsDb keep their Windows names for file compatibility;
// the macOS equivalents (.DS_Store etc.) are covered by the same toggles.
section!(EmptyFolderSettings {
    ignore_zero_byte_files: bool = false,
    ignore_desktop_ini: bool = true,
    ignore_thumbs_db: bool = true,
});
section!(TreeSettings {
    folders_only: bool = false,
    exclusions: ExclusionRules = ExclusionRules::default()
});
section!(AnalyzeSettings {
    save_format: ReportFormat = ReportFormat::Csv,
    exclusions: ExclusionRules = ExclusionRules::default()
});
section!(ArchiveSettings {
    compress_format: ArchiveCompressFormat = ArchiveCompressFormat::Zip
});
section!(FolderSettings {
    duplicates: DuplicateSettings = DuplicateSettings::default(),
    delete_method: DeleteMethod = DeleteMethod::RecycleBin,
    scan_exclusions: ExclusionRules = ExclusionRules::default(),
    rename: RenameSettings = RenameSettings::default(),
    empty: EmptyFolderSettings = EmptyFolderSettings::default(),
    tree: TreeSettings = TreeSettings::default(),
    analyze: AnalyzeSettings = AnalyzeSettings::default(),
    archive: ArchiveSettings = ArchiveSettings::default(),
});
section!(VideoPresets {
    resize_a: f64 = 50.0,
    resize_b: f64 = 25.0,
    compress_discord: i64 = DISCORD_LIMIT,
    compress_a: i64 = 50 * MIB,
    compress_b: i64 = 25 * MIB,
    compress_c: i64 = 5 * MIB,
    rotate_a: f64 = 45.0,
    rotate_b: f64 = 90.0,
    rotate_c: f64 = 180.0,
});
section!(TrimSettings {
    accuracy: TrimAccuracy = TrimAccuracy::Keyframe
});
section!(ThumbnailSettings {
    format: ThumbnailFormat = ThumbnailFormat::Png,
    quality: i32 = 90,
    max_width: i32 = 0
});
section!(SubtitlesSettings {
    model: SubtitleModel = SubtitleModel::Small,
    device: SubtitleDevice = SubtitleDevice::Auto,
    language: String = String::new(),
    output: SubtitleOutput = SubtitleOutput::Srt,
});
section!(VideoSettings {
    presets: VideoPresets = VideoPresets::default(),
    trim: TrimSettings = TrimSettings::default(),
    thumbnail: ThumbnailSettings = ThumbnailSettings::default(),
    codec: VideoCodecChoice = VideoCodecChoice::H264,
    container: VideoContainer = VideoContainer::Auto,
    hardware: HardwareEncoder = HardwareEncoder::None,
    priority: EncodePriority = EncodePriority::Balanced,
    gif_fps: i32 = 15,
    gif_max_width: i32 = 640,
    gif_dither: GifDither = GifDither::Bayer,
    rotate_fill_color: String = "#000000".into(),
    subtitles: SubtitlesSettings = SubtitlesSettings::default(),
});
section!(ImagePresets {
    resize_a: f64 = 50.0,
    resize_b: f64 = 25.0,
    compress_a: i64 = 10 * MIB,
    compress_b: i64 = 5 * MIB,
    compress_c: i64 = 3 * MIB,
    compress_d: i64 = MIB,
    rotate_a: f64 = 45.0,
    rotate_b: f64 = 90.0,
    rotate_c: f64 = 180.0,
});
section!(MetadataSettings {
    remove_exif: bool = true,
    remove_gps: bool = true,
    remove_comments: bool = true,
    remove_color_profile: bool = false,
    apply_orientation: bool = true,
});
section!(ImageSettings {
    presets: ImagePresets = ImagePresets::default(),
    allow_webp_fallback: bool = true,
    webp_quality: i32 = 80,
    background_mode: BackgroundRemovalMode = BackgroundRemovalMode::AiWithFallback,
    background_key_color: String = "#00FF00".into(),
    background_tolerance: f64 = 25.0,
    metadata: MetadataSettings = MetadataSettings::default(),
});
section!(JsonSortSettings {
    comparison: JsonKeyOrder = JsonKeyOrder::Ordinal,
    descending: bool = false,
    recursive: bool = true,
    sort_primitive_arrays: bool = false,
    pinned_keys: String = String::new(),
});
section!(TextSettings {
    sjis_fallback: SjisFallback = SjisFallback::ReplaceAndWarn,
    sjis_substitute: String = "?".into(),
    json_sort: JsonSortSettings = JsonSortSettings::default(),
});
section!(OcrSettings {
    language: String = String::new(),
    output: OcrOutput = OcrOutput::TextFile,
    encoding: TextEncodingChoice = TextEncodingChoice::Utf8Bom,
    suppress_language_notice: bool = false,
});
section!(DecompileSettings {
    output_path: String = String::new()
});
section!(CodeSettings {
    decompile: DecompileSettings = DecompileSettings::default()
});
// Not in the Windows app. Off until the user turns it on and installs yt-dlp themselves.
section!(DownloadSettings {
    enabled: bool = false
});

// Not in the Windows app: one output folder per feature. Empty = beside the original; a
// relative path starts from the original's folder; `~` and absolute paths work. Windows
// files simply lack the section, which loads as all-empty.
section!(OutputSettings {
    image: String = String::new(),
    video: String = String::new(),
    audio: String = String::new(),
    text: String = String::new(),
    pdf: String = String::new(),
    ocr: String = String::new(),
    archive_extract: String = String::new(),
    archive_compress: String = String::new(),
    report: String = String::new(),
    // Empty = ~/Downloads (there is no source file to put it beside).
    download: String = String::new(),
});

impl OutputSettings {
    /// The folder setting for a task id such as `image.resize` or `video.extractAudio`.
    pub fn for_task(&self, task: &str) -> &str {
        match task {
            "video.extractAudio" => &self.audio,
            "archive.extract" => &self.archive_extract,
            "folder.compress" => &self.archive_compress,
            "folder.analyze" => &self.report,
            "image.ocr" => &self.ocr,
            t => match t.split_once('.').map_or(t, |(group, _)| group) {
                "image" => &self.image,
                "video" => &self.video,
                "audio" => &self.audio,
                "text" => &self.text,
                "pdf" => &self.pdf,
                "download" => &self.download,
                _ => "",
            },
        }
    }
}

pub const SCHEMA_VERSION: i32 = 1;

section!(Settings {
    version: i32 = 0,
    general: GeneralSettings = GeneralSettings::default(),
    folder: FolderSettings = FolderSettings::default(),
    video: VideoSettings = VideoSettings::default(),
    image: ImageSettings = ImageSettings::default(),
    text: TextSettings = TextSettings::default(),
    ocr: OcrSettings = OcrSettings::default(),
    code: CodeSettings = CodeSettings::default(),
    download: DownloadSettings = DownloadSettings::default(),
    output: OutputSettings = OutputSettings::default(),
});

impl Settings {
    fn upgrade(&mut self) {
        if self.video.presets.compress_discord == LEGACY_DISCORD_LIMIT {
            self.video.presets.compress_discord = DISCORD_LIMIT;
        }
    }
}

const FILE_NAME: &str = "settings.json";
const BROKEN_SUFFIX: &str = ".broken";
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
const STALE_AFTER: Duration = Duration::from_secs(3600);

/// Atomic, permission-restricted JSON store in one directory.
pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn load(&self) -> Settings {
        let path = self.dir.join(FILE_NAME);
        let Ok(meta) = fs::metadata(&path) else {
            return Settings::default();
        };
        if meta.len() > MAX_FILE_BYTES {
            set_aside(&path);
            return Settings::default();
        }
        let parsed = fs::read(&path).ok().and_then(|bytes| {
            let text = String::from_utf8(bytes).ok()?;
            serde_json::from_str::<Settings>(text.trim_start_matches('\u{feff}')).ok()
        });
        match parsed {
            None => {
                set_aside(&path);
                Settings::default()
            }
            Some(mut s) if s.version < SCHEMA_VERSION => {
                s.upgrade();
                s.version = SCHEMA_VERSION;
                let _ = self.save(&s);
                s
            }
            Some(s) => s,
        }
    }

    pub fn save(&self, settings: &Settings) -> io::Result<()> {
        let mut settings = settings.clone();
        settings.version = SCHEMA_VERSION;
        self.write_atomic(FILE_NAME, &serde_json::to_vec_pretty(&settings)?)
    }

    pub fn reset(&self) {
        let _ = fs::remove_file(self.dir.join(FILE_NAME));
    }

    fn write_atomic(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        sweep_stale(&self.dir, name);
        // Follow a symlinked settings file so the link keeps working.
        let target = fs::canonicalize(self.dir.join(name)).unwrap_or_else(|_| self.dir.join(name));
        let folder = target.parent().unwrap_or(&self.dir);
        let temp = folder.join(format!(".{name}.{}.tmp", unique_token()));
        let result = (|| {
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp)?;
            f.write_all(bytes)?;
            f.sync_all()?;
            fs::rename(&temp, &target)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }
}

fn set_aside(path: &Path) {
    let mut broken = path.as_os_str().to_owned();
    broken.push(BROKEN_SUFFIX);
    let _ = fs::rename(path, broken);
}

fn sweep_stale(dir: &Path, name: &str) {
    let prefix = format!(".{name}.");
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if !(n.starts_with(&prefix) && n.ends_with(".tmp")) {
            continue;
        }
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| SystemTime::now().duration_since(m).ok())
            .is_some_and(|age| age > STALE_AFTER);
        if old {
            let _ = fs::remove_file(e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn defaults_match_windows() {
        let s = Settings::default();
        assert_eq!(s.image.presets.compress_a, 10 * MIB);
        assert_eq!(s.video.presets.compress_discord, 20 * MIB);
        assert_eq!(s.ocr.encoding, TextEncodingChoice::Utf8Bom);
        assert!(s.text.json_sort.recursive);
    }

    #[test]
    fn download_is_off_by_default_and_old_files_still_load() {
        assert!(!Settings::default().download.enabled);
        let s: Settings = serde_json::from_str("{\"General\":{}}").unwrap();
        assert!(!s.download.enabled);
        let on: Settings = serde_json::from_str("{\"Download\":{\"Enabled\":true}}").unwrap();
        assert!(on.download.enabled);
    }

    #[test]
    fn round_trip_with_private_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("sub"));
        let mut s = Settings::default();
        s.general.hidden_menu_items = vec!["image.ocr".into()];
        s.video.hardware = HardwareEncoder::VideoToolbox;
        store.save(&s).unwrap();
        let loaded = store.load();
        assert_eq!(loaded.general.hidden_menu_items, ["image.ocr"]);
        assert_eq!(loaded.video.hardware, HardwareEncoder::VideoToolbox);
        assert_eq!(loaded.version, SCHEMA_VERSION);
        let mode = fs::metadata(dir.path().join("sub/settings.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn loads_a_windows_file_with_bom_and_missing_fields() {
        let dir = tempfile::tempdir().unwrap();
        let json = "\u{feff}{\"Version\":0,\"General\":{\"Language\":\"Japanese\"},\"Video\":{\"Hardware\":\"Nvenc\",\"Presets\":{\"CompressDiscord\":10485760}}}";
        fs::write(dir.path().join("settings.json"), json).unwrap();
        let s = Store::new(dir.path().into()).load();
        assert_eq!(s.general.language, AppLanguage::Japanese);
        assert!(s.general.close_on_success); // default filled in
        assert_eq!(s.video.hardware, HardwareEncoder::Nvenc);
        assert_eq!(s.video.presets.compress_discord, DISCORD_LIMIT); // legacy upgrade
    }

    #[test]
    fn broken_or_huge_files_are_set_aside() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().into());
        fs::write(dir.path().join("settings.json"), "{ nope").unwrap();
        assert_eq!(store.load(), Settings::default());
        assert!(dir.path().join("settings.json.broken").exists());
        assert!(!dir.path().join("settings.json").exists());

        fs::write(
            dir.path().join("settings.json"),
            vec![b' '; (MAX_FILE_BYTES + 1) as usize],
        )
        .unwrap();
        assert_eq!(store.load(), Settings::default());
    }

    #[test]
    fn writes_through_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.json");
        fs::write(&real, "{}").unwrap();
        std::os::unix::fs::symlink(&real, dir.path().join("settings.json")).unwrap();
        Store::new(dir.path().into())
            .save(&Settings::default())
            .unwrap();
        assert!(dir
            .path()
            .join("settings.json")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(fs::read_to_string(real).unwrap().contains("\"Version\""));
    }
}
