//! Media download through the user's own `yt-dlp` (see NOTICES.md: not bundled, not
//! fetched by Flect). Off by default (`Settings.download.enabled`).
//!
//! Only builds and checks the command line; it never goes through a shell. Nothing here
//! handles logins, cookies or DRM, and the argument builder takes no free-form options,
//! so a caller cannot smuggle a yt-dlp option in through a URL or a format string.
use std::path::{Path, PathBuf};

use crate::archive::find_tool;

pub const TOOL_NAME: &str = "yt-dlp";
const MAX_URL_BYTES: usize = 2048;
/// Longest video height accepted for a quality cap (8K).
const MAX_HEIGHT: u32 = 4320;

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

/// Arguments for one download into `out_dir`. `url` must already have passed `validate_url`.
/// The address comes after `--`, so it is never read as an option.
pub fn build_args(url: &str, kind: Kind, out_dir: &Path) -> Vec<String> {
    let mut args: Vec<String> = [
        // Ignore the user's own yt-dlp config files so results do not depend on them.
        "--ignore-config",
        "--no-playlist",
        "-P",
    ]
    .map(String::from)
    .to_vec();
    args.push(out_dir.to_string_lossy().into_owned());
    args.push("-o".into());
    args.push("%(title).100B [%(id)s].%(ext)s".into());
    match kind {
        Kind::Audio => {
            args.extend(["-x", "--audio-format", "mp3"].map(String::from));
        }
        Kind::Video { max_height } => {
            let selector = match max_height.filter(|h| (1..=MAX_HEIGHT).contains(h)) {
                Some(h) => format!("bv*[height<={h}]+ba/b[height<={h}]"),
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn video_arguments() {
        let a = build_args(
            "https://example.com/v",
            Kind::Video {
                max_height: Some(720),
            },
            Path::new("/tmp/out"),
        );
        assert_eq!(
            a,
            [
                "--ignore-config",
                "--no-playlist",
                "-P",
                "/tmp/out",
                "-o",
                "%(title).100B [%(id)s].%(ext)s",
                "-f",
                "bv*[height<=720]+ba/b[height<=720]",
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
            );
            assert!(a.contains(&"bv*+ba/b".to_string()), "{h:?}");
        }
    }

    #[test]
    fn audio_arguments() {
        let a = build_args("https://example.com/v", Kind::Audio, Path::new("/o"));
        assert!(a.windows(3).any(|w| w == ["-x", "--audio-format", "mp3"]));
        assert!(!a.iter().any(|s| s == "-f"));
    }

    #[test]
    fn the_address_always_follows_the_option_terminator() {
        for kind in [Kind::Audio, Kind::Video { max_height: None }] {
            let a = build_args("https://example.com/v", kind, Path::new("/o"));
            let n = a.len();
            assert_eq!(a[n - 2], "--");
            assert_eq!(a[n - 1], "https://example.com/v");
        }
    }
}
