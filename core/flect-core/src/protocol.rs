//! UI <-> engine protocol: one JSON object per line over stdio.
//! Inputs are bounded and strictly validated; unknown fields are rejected.
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::pipeline;

pub const MAX_LINE_BYTES: usize = 1 << 20; // 1 MiB
pub const MAX_PATHS: usize = 20_000;
pub const MAX_PATH_BYTES: usize = 4096;

#[derive(Debug, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Request {
    Run {
        task: String,
        paths: Vec<String>,
        #[serde(default)]
        options: Value,
    },
    /// Steps chained on each path; only the last step's result is written next to the original.
    Pipeline {
        steps: Vec<String>,
        paths: Vec<String>,
    },
    Cancel,
    ExportMenu,
}

/// Localization is the UI's job: events carry message keys and arguments only.
#[derive(Debug, Serialize, PartialEq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Status {
        key: String,
        args: Vec<String>,
    },
    Step {
        done: u64,
        total: u64,
    },
    /// Progress inside the file currently being processed (0..=1000).
    FileStep {
        permille: u32,
    },
    Info {
        key: String,
        args: Vec<String>,
    },
    Warn {
        key: String,
        args: Vec<String>,
    },
    Error {
        key: String,
        args: Vec<String>,
    },
    /// The engine needs the UI to do an OS-native step (e.g. trash, OCR).
    Needs {
        what: String,
        paths: Vec<String>,
    },
    Menu {
        menu: Value,
    },
    /// A result for the UI to present (tree text, analysis, duplicate groups, rename preview...).
    Data {
        name: String,
        data: Value,
    },
    Done {
        outcome: Outcome,
        summary_key: Option<String>,
        summary_args: Vec<String>,
    },
}

#[derive(Debug, Serialize, PartialEq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    TooLong,
    NotUtf8,
    Json(String),
    TooManyPaths,
    BadPath,
    BadPipeline,
}

impl ParseError {
    pub fn key(&self) -> &'static str {
        match self {
            Self::TooLong => "protocol.lineTooLong",
            Self::NotUtf8 => "protocol.notUtf8",
            Self::Json(_) => "protocol.badJson",
            Self::TooManyPaths => "protocol.tooManyPaths",
            Self::BadPath => "protocol.badPath",
            Self::BadPipeline => "protocol.badPipeline",
        }
    }
}

pub fn parse_line(line: &[u8]) -> Result<Request, ParseError> {
    if line.len() > MAX_LINE_BYTES {
        return Err(ParseError::TooLong);
    }
    let text = std::str::from_utf8(line).map_err(|_| ParseError::NotUtf8)?;
    let value: Value =
        serde_json::from_str(text.trim()).map_err(|e| ParseError::Json(e.to_string()))?;
    // serde cannot deny unknown fields on unit variants of a tagged enum; do it here.
    if matches!(
        value.get("op").and_then(Value::as_str),
        Some("cancel" | "export-menu")
    ) && value.as_object().is_some_and(|o| o.len() != 1)
    {
        return Err(ParseError::Json("unknown field".into()));
    }
    let request: Request =
        serde_json::from_value(value).map_err(|e| ParseError::Json(e.to_string()))?;
    match &request {
        Request::Run { paths, .. } => check_paths(paths)?,
        Request::Pipeline { steps, paths } => {
            // Only the bounds here; what each step may be is checked by `pipeline::plan`.
            if steps.is_empty()
                || steps.len() > pipeline::MAX_STEPS
                || steps
                    .iter()
                    .any(|s| s.is_empty() || s.len() > pipeline::MAX_STEP_BYTES || s.contains('\0'))
            {
                return Err(ParseError::BadPipeline);
            }
            check_paths(paths)?
        }
        Request::Cancel | Request::ExportMenu => {}
    }
    Ok(request)
}

fn check_paths(paths: &[String]) -> Result<(), ParseError> {
    if paths.is_empty() || paths.len() > MAX_PATHS {
        return Err(ParseError::TooManyPaths);
    }
    // Absolute, no NUL, bounded: relative paths would depend on our cwd.
    if paths
        .iter()
        .any(|p| !p.starts_with('/') || p.contains('\0') || p.len() > MAX_PATH_BYTES)
    {
        return Err(ParseError::BadPath);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_requests() {
        assert_eq!(
            parse_line(br#"{"op":"export-menu"}"#),
            Ok(Request::ExportMenu)
        );
        assert_eq!(parse_line(br#"{"op":"cancel"}"#), Ok(Request::Cancel));
        let r =
            parse_line(r#"{"op":"run","task":"image.ocr","paths":["/tmp/写真.png"]}"#.as_bytes())
                .unwrap();
        assert!(matches!(r, Request::Run { ref task, .. } if task == "image.ocr"));
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(
            parse_line(&vec![b' '; MAX_LINE_BYTES + 1]),
            Err(ParseError::TooLong)
        );
        assert_eq!(parse_line(&[0xff, 0xfe]), Err(ParseError::NotUtf8));
        assert!(matches!(parse_line(b"{"), Err(ParseError::Json(_))));
        assert!(matches!(
            parse_line(br#"{"op":"cancel","x":1}"#),
            Err(ParseError::Json(_))
        ));
        assert!(matches!(
            parse_line(br#"{"op":"nope"}"#),
            Err(ParseError::Json(_))
        ));
        assert_eq!(
            parse_line(br#"{"op":"run","task":"a","paths":[]}"#),
            Err(ParseError::TooManyPaths)
        );
        assert_eq!(
            parse_line(br#"{"op":"run","task":"a","paths":["rel/x"]}"#),
            Err(ParseError::BadPath)
        );
        assert_eq!(
            parse_line(br#"{"op":"run","task":"a","paths":["/a\u0000b"]}"#),
            Err(ParseError::BadPath)
        );
    }

    #[test]
    fn pipeline_requests_are_bounded() {
        let r = parse_line(
            br#"{"op":"pipeline","steps":["image.resize?scale=50","image.strip"],"paths":["/a.png"]}"#,
        )
        .unwrap();
        assert!(matches!(r, Request::Pipeline { ref steps, .. } if steps.len() == 2));
        let nine = ["\"image.strip\""; pipeline::MAX_STEPS + 1].join(",");
        let long = "a".repeat(pipeline::MAX_STEP_BYTES + 1);
        for bad in [
            r#"{"op":"pipeline","steps":[],"paths":["/a"]}"#.to_owned(),
            format!(r#"{{"op":"pipeline","steps":[{nine}],"paths":["/a"]}}"#),
            r#"{"op":"pipeline","steps":[""],"paths":["/a"]}"#.to_owned(),
            format!(r#"{{"op":"pipeline","steps":["{long}"],"paths":["/a"]}}"#),
        ] {
            assert_eq!(
                parse_line(bad.as_bytes()),
                Err(ParseError::BadPipeline),
                "{bad}"
            );
        }
        assert_eq!(
            parse_line(br#"{"op":"pipeline","steps":["image.strip"],"paths":["rel"]}"#),
            Err(ParseError::BadPath)
        );
        assert_eq!(
            parse_line(br#"{"op":"pipeline","steps":["image.strip"],"paths":[]}"#),
            Err(ParseError::TooManyPaths)
        );
        assert!(matches!(
            parse_line(br#"{"op":"pipeline","steps":["image.strip"],"paths":["/a"],"x":1}"#),
            Err(ParseError::Json(_))
        ));
        assert!(matches!(
            parse_line(br#"{"op":"pipeline","paths":["/a"]}"#),
            Err(ParseError::Json(_))
        ));
    }

    #[test]
    fn events_serialize_tagged() {
        let s = serde_json::to_string(&Event::Done {
            outcome: Outcome::Cancelled,
            summary_key: None,
            summary_args: vec![],
        })
        .unwrap();
        assert_eq!(
            s,
            r#"{"event":"done","outcome":"cancelled","summary_key":null,"summary_args":[]}"#
        );
    }
}
