//! Speech to subtitles through ffmpeg's `whisper` filter (whisper.cpp), with no Python.
//!
//! Security: the model is a large binary that whisper.cpp parses, so only models whose SHA-256 is
//! pinned below are accepted. They are installed by `scripts/fetch-whisper-model.sh` (which checks the
//! same hashes), never downloaded by the engine. The filter is given plain relative names
//! (`model.bin`, `out.srt`) and a scratch working directory, so no path is ever escaped into a filter graph.
use std::path::PathBuf;

use crate::locations;
use crate::settings::SubtitleModel;

pub struct Model {
    pub file: &'static str,
    pub sha256: &'static str,
}

/// Multilingual ggml models from <https://huggingface.co/ggerganov/whisper.cpp> (hashes from the repository's LFS metadata).
pub fn model_for(m: SubtitleModel) -> Model {
    match m {
        SubtitleModel::Tiny => Model {
            file: "ggml-tiny.bin",
            sha256: "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21",
        },
        SubtitleModel::Base => Model {
            file: "ggml-base.bin",
            sha256: "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
        },
        SubtitleModel::Small => Model {
            file: "ggml-small.bin",
            sha256: "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b",
        },
        SubtitleModel::Medium => Model {
            file: "ggml-medium.bin",
            sha256: "6c14d5adee5f86394037b4e4e8b59f1673b6cee10e3cf0b11bbdbee79c156208",
        },
        SubtitleModel::LargeV3 => Model {
            file: "ggml-large-v3.bin",
            sha256: "64d182b440b98d5203c4f9bd541544d84c605196c4f7b845dfa11fb23594d1e2",
        },
        SubtitleModel::Turbo => Model {
            file: "ggml-large-v3-turbo.bin",
            sha256: "1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69",
        },
    }
}

pub fn model_dir() -> Option<PathBuf> {
    locations::data_dir().map(|d| d.join("deps/whisper"))
}

/// "" or "auto" -> "auto"; otherwise a 2-3 letter language code. Anything else is refused.
pub fn language_arg(setting: &str) -> Option<String> {
    let l = setting.trim().to_lowercase();
    if l.is_empty() || l == "auto" {
        return Some("auto".into());
    }
    (matches!(l.len(), 2 | 3) && l.bytes().all(|b| b.is_ascii_lowercase())).then_some(l)
}

/// The `-af` value. `format` is the filter's own: `srt` or `text`.
pub fn filter_arg(language: &str, use_gpu: bool, format: &str) -> String {
    format!(
        "whisper=model=model.bin:language={language}:queue=3:use_gpu={}:destination=out.{}:format={format}",
        use_gpu,
        if format == "text" { "txt" } else { "srt" }
    )
}

/// SubRip -> WebVTT: header, and `,` -> `.` in the timing lines.
pub fn srt_to_vtt(srt: &str) -> String {
    let mut out = String::from("WEBVTT\n\n");
    for line in srt.replace("\r\n", "\n").lines() {
        if line.contains("-->") {
            out.push_str(&line.replace(',', "."));
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages_are_codes_only() {
        assert_eq!(language_arg(""), Some("auto".into()));
        assert_eq!(language_arg(" JA "), Some("ja".into()));
        assert_eq!(language_arg("auto"), Some("auto".into()));
        assert_eq!(language_arg("en:x=1"), None);
        assert_eq!(language_arg("japanese"), None);
    }

    #[test]
    fn filter_uses_only_relative_names() {
        let f = filter_arg("ja", false, "srt");
        assert!(f.contains("model=model.bin") && f.contains("destination=out.srt"));
        assert!(!f.contains('/'));
        assert!(filter_arg("auto", true, "text").contains("destination=out.txt"));
    }

    #[test]
    fn vtt_conversion() {
        let vtt = srt_to_vtt("1\r\n00:00:01,500 --> 00:00:02,250\r\nHello, world\r\n\r\n");
        assert!(vtt.starts_with("WEBVTT\n\n"));
        assert!(vtt.contains("00:00:01.500 --> 00:00:02.250"));
        assert!(vtt.contains("Hello, world"));
    }

    #[test]
    fn every_model_has_a_pinned_hash() {
        use SubtitleModel::*;
        for m in [Tiny, Base, Small, Medium, LargeV3, Turbo] {
            let model = model_for(m);
            assert_eq!(model.sha256.len(), 64);
            assert!(model.file.starts_with("ggml-") && model.file.ends_with(".bin"));
        }
    }
}
