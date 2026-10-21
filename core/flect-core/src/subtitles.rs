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

/// One subtitle cue, in milliseconds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cue {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

fn parse_timestamp(t: &str) -> Option<u64> {
    let t = t.trim().replace('.', ",");
    let (hms, ms) = t.split_once(',')?;
    let mut parts = hms.split(':');
    let (h, m, sec) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let ms: u64 = format!("{:0<3}", ms.trim()).get(..3)?.parse().ok()?;
    Some(
        ((h.trim().parse::<u64>().ok()? * 60 + m.trim().parse::<u64>().ok()?) * 60
            + sec.trim().parse::<u64>().ok()?)
            * 1000
            + ms,
    )
}

/// Cues from the filter's SubRip output (whatever numbering it used is ignored).
pub fn parse_srt(srt: &str) -> Vec<Cue> {
    let mut cues = Vec::new();
    for block in srt.replace("\r\n", "\n").split("\n\n") {
        let mut lines = block
            .lines()
            .map(str::trim_end)
            .skip_while(|l| l.trim().is_empty());
        let mut timing = None;
        let mut text = Vec::new();
        for l in lines.by_ref() {
            if timing.is_none() {
                if let Some((a, b)) = l.split_once("-->") {
                    if let (Some(s), Some(e)) = (parse_timestamp(a), parse_timestamp(b)) {
                        timing = Some((s, e));
                    }
                }
                continue; // cue numbers and noise before the timing line
            }
            text.push(l);
        }
        if let Some((start_ms, end_ms)) = timing {
            let text = text.join("\n").trim().to_owned();
            if !text.is_empty() {
                cues.push(Cue {
                    start_ms,
                    end_ms,
                    text,
                });
            }
        }
    }
    cues
}

/// Keep cues inside the media: whisper can emit an end time past the end of the audio, or a cue
/// after it. `duration_ms == 0` (unknown) leaves the times alone.
pub fn normalize(cues: Vec<Cue>, duration_ms: u64) -> Vec<Cue> {
    cues.into_iter()
        .filter(|c| duration_ms == 0 || c.start_ms < duration_ms)
        .map(|mut c| {
            if duration_ms > 0 {
                c.end_ms = c.end_ms.min(duration_ms);
            }
            if c.end_ms <= c.start_ms {
                c.end_ms = c.start_ms + 1;
            }
            c
        })
        .collect()
}

fn stamp(ms: u64, sep: char) -> String {
    format!(
        "{:02}:{:02}:{:02}{sep}{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

/// SubRip, numbered from 1.
pub fn render_srt(cues: &[Cue]) -> String {
    let mut out = String::new();
    for (i, c) in cues.iter().enumerate() {
        out.push_str(&format!(
            "{}\n{} --> {}\n{}\n\n",
            i + 1,
            stamp(c.start_ms, ','),
            stamp(c.end_ms, ','),
            c.text
        ));
    }
    out
}

/// WebVTT, without cue numbers.
pub fn render_vtt(cues: &[Cue]) -> String {
    let mut out = String::from("WEBVTT\n\n");
    for c in cues {
        out.push_str(&format!(
            "{} --> {}\n{}\n\n",
            stamp(c.start_ms, '.'),
            stamp(c.end_ms, '.'),
            c.text
        ));
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

    const RAW: &str = "0\r\n00:00:01,500 --> 00:00:02,250\r\nHello, world\r\n\r\n1\r\n00:00:03,000 --> 00:00:32,000\r\nsecond\nline\r\n\r\n2\r\n00:00:40,000 --> 00:00:41,000\r\nlate\r\n\r\n";

    #[test]
    fn cues_are_renumbered_and_kept_inside_the_media() {
        let cues = normalize(parse_srt(RAW), 7_400);
        assert_eq!(cues.len(), 2, "the cue after the end is dropped: {cues:?}");
        assert_eq!(cues[1].end_ms, 7_400);
        let srt = render_srt(&cues);
        assert!(srt.starts_with("1\n00:00:01,500 --> 00:00:02,250\nHello, world\n\n2\n"));
        assert!(srt.contains("00:00:03,000 --> 00:00:07,400\nsecond\nline"));
    }

    #[test]
    fn vtt_has_no_cue_numbers_and_dot_milliseconds() {
        let vtt = render_vtt(&normalize(parse_srt(RAW), 0));
        assert!(vtt.starts_with("WEBVTT\n\n00:00:01.500 --> 00:00:02.250\nHello, world"));
        assert!(!vtt.contains("\n1\n"));
        assert!(
            vtt.contains("00:00:40.000 --> 00:00:41.000"),
            "unknown duration keeps every cue"
        );
    }

    #[test]
    fn zero_length_cues_get_one_millisecond() {
        let c = normalize(parse_srt("1\n00:00:05,000 --> 00:00:05,000\nx\n"), 10_000);
        assert_eq!((c[0].start_ms, c[0].end_ms), (5_000, 5_001));
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
