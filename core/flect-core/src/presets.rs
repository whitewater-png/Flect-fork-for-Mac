//! `presets.json`: named step lists ("SNS 1080px / 2MB" = resize + compress). The app writes the
//! file; the CLI only reads it. It sits in the user's data folder and is read as untrusted input:
//! every field is checked, and one bad entry makes the whole file invalid (a partial read that
//! was later saved would silently drop the rest). The Swift `PresetFile.decode` applies the
//! same rules; both are tested against `tests/fixtures/presets/`.
use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use crate::pipeline;

pub const FILE_NAME: &str = "presets.json";
pub const SCHEMA_VERSION: u64 = 1;
pub const MAX_FILE_BYTES: u64 = 256 * 1024;
pub const MAX_PRESETS: usize = 100;
pub const MAX_NAME_CHARS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preset {
    pub id: String,
    pub name: String,
    /// 1..=pipeline::MAX_STEPS task strings, already checked as a chain.
    pub steps: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PresetError {
    /// No presets file (nothing saved yet).
    Missing,
    Unreadable(String),
    TooLarge,
    /// Written by a newer version: left alone, never "repaired".
    Newer(u64),
    Invalid(String),
}

impl std::fmt::Display for PresetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => write!(f, "no presets have been saved yet"),
            Self::Unreadable(e) => write!(f, "cannot read the presets file: {e}"),
            Self::TooLarge => write!(f, "the presets file is larger than {MAX_FILE_BYTES} bytes"),
            Self::Newer(v) => write!(f, "the presets file is from a newer version (schema {v})"),
            Self::Invalid(e) => write!(f, "the presets file is invalid: {e}"),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    #[allow(dead_code)]
    version: u64,
    presets: Vec<RawPreset>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPreset {
    id: String,
    name: String,
    steps: Vec<String>,
}

/// Canonical 8-4-4-4-12 hex UUID (either case).
pub fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

/// Shown in menus and matched by `--preset`: no padding, bounded, and nothing invisible or
/// direction-changing that could make two names look alike or hide text.
pub fn is_valid_name(name: &str) -> bool {
    let count = name.chars().count();
    (1..=MAX_NAME_CHARS).contains(&count)
        && name.trim() == name
        && !name.chars().any(|c| {
            c.is_control()
                || matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}')
        })
}

/// Parse and check the whole file.
pub fn parse(bytes: &[u8]) -> Result<Vec<Preset>, PresetError> {
    let invalid = |m: &str| PresetError::Invalid(m.to_owned());
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(PresetError::TooLarge);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| invalid("not UTF-8"))?;
    let value: Value =
        serde_json::from_str(text).map_err(|e| PresetError::Invalid(e.to_string()))?;
    // The version decides how to read the rest, so it is looked at before the strict parse.
    let version = value
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid("missing or bad version"))?;
    if version > SCHEMA_VERSION {
        return Err(PresetError::Newer(version));
    }
    if version != SCHEMA_VERSION {
        return Err(invalid("unsupported version"));
    }
    let raw: RawFile =
        serde_json::from_value(value).map_err(|e| PresetError::Invalid(e.to_string()))?;
    if raw.presets.len() > MAX_PRESETS {
        return Err(invalid("too many presets"));
    }
    let mut ids = std::collections::HashSet::new();
    let mut names = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(raw.presets.len());
    for (i, p) in raw.presets.into_iter().enumerate() {
        let at = |what: &str| PresetError::Invalid(format!("preset {}: {what}", i + 1));
        if !is_uuid(&p.id) || !ids.insert(p.id.to_ascii_uppercase()) {
            return Err(at("bad or duplicate id"));
        }
        if !is_valid_name(&p.name) || !names.insert(p.name.to_lowercase()) {
            return Err(at("bad or duplicate name"));
        }
        if let Err(e) = pipeline::plan(&p.steps) {
            return Err(at(&format!("{} {:?}", e.key(), e.args())));
        }
        out.push(Preset {
            id: p.id,
            name: p.name,
            steps: p.steps,
        });
    }
    Ok(out)
}

/// Read `<dir>/presets.json` without ever changing it.
pub fn load(dir: &Path) -> Result<Vec<Preset>, PresetError> {
    let path = dir.join(FILE_NAME);
    let meta = match fs::metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(PresetError::Missing),
        Err(e) => return Err(PresetError::Unreadable(e.to_string())),
    };
    // Checked before opening: opening a FIFO planted under the name would block.
    if !meta.is_file() {
        return Err(PresetError::Invalid("not a regular file".into()));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(PresetError::TooLarge);
    }
    let file = File::open(&path).map_err(|e| PresetError::Unreadable(e.to_string()))?;
    if !file.metadata().is_ok_and(|m| m.is_file()) {
        return Err(PresetError::Invalid("not a regular file".into()));
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| PresetError::Unreadable(e.to_string()))?;
    parse(&bytes)
}

/// `--preset KEY`: the preset with exactly that name, else the one with that id.
pub fn find<'a>(presets: &'a [Preset], key: &str) -> Option<&'a Preset> {
    presets
        .iter()
        .find(|p| p.name == key)
        .or_else(|| presets.iter().find(|p| p.id.eq_ignore_ascii_case(key)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/presets")
    }

    fn fixture_files(prefix: &str) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = fs::read_dir(fixtures())
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with(prefix))
            .collect();
        v.sort();
        assert!(!v.is_empty(), "no {prefix} fixtures");
        v
    }

    /// The same files are read by the Swift tests, so both parsers agree.
    #[test]
    fn shared_fixtures() {
        for f in fixture_files("valid-") {
            let r = parse(&fs::read(&f).unwrap());
            assert!(r.is_ok(), "{}: {r:?}", f.display());
        }
        for f in fixture_files("invalid-") {
            let r = parse(&fs::read(&f).unwrap());
            assert!(
                matches!(r, Err(PresetError::Invalid(_))),
                "{}: {r:?}",
                f.display()
            );
        }
        for f in fixture_files("newer-") {
            assert!(matches!(
                parse(&fs::read(&f).unwrap()),
                Err(PresetError::Newer(_))
            ));
        }
    }

    #[test]
    fn valid_file_reads_in_order() {
        let p = parse(&fs::read(fixtures().join("valid-basic.json")).unwrap()).unwrap();
        assert_eq!(p[0].name, "SNS 1080px / 2MB");
        assert_eq!(p[0].steps.len(), 2);
        assert_eq!(find(&p, "SNS 1080px / 2MB").unwrap().id, p[0].id);
        assert_eq!(find(&p, &p[1].id.to_lowercase()).unwrap().name, p[1].name);
        assert!(
            find(&p, "sns 1080px / 2mb").is_none(),
            "names match exactly"
        );
    }

    #[test]
    fn names() {
        assert!(is_valid_name("写真 → Web"));
        assert!(is_valid_name(&"a".repeat(MAX_NAME_CHARS)));
        for bad in [
            "",
            " a",
            "a ",
            "a\nb",
            "a\u{202E}b",
            "a\u{200B}",
            &"a".repeat(MAX_NAME_CHARS + 1),
        ] {
            assert!(!is_valid_name(bad), "{bad:?}");
        }
        assert!(is_uuid("6F1C2A8E-0B7D-4C1E-9A55-3D2B1C0E9F11"));
        assert!(!is_uuid("6F1C2A8E-0B7D-4C1E-9A55-3D2B1C0E9F1"));
        assert!(!is_uuid("6F1C2A8E+0B7D-4C1E-9A55-3D2B1C0E9F11"));
    }

    #[test]
    fn load_is_read_only_and_refuses_odd_files() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(load(d.path()), Err(PresetError::Missing));
        fs::create_dir(d.path().join(FILE_NAME)).unwrap();
        assert!(matches!(load(d.path()), Err(PresetError::Invalid(_))));
        fs::remove_dir(d.path().join(FILE_NAME)).unwrap();
        fs::write(d.path().join(FILE_NAME), "{ broken").unwrap();
        assert!(matches!(load(d.path()), Err(PresetError::Invalid(_))));
        // nothing was renamed or rewritten
        assert_eq!(fs::read(d.path().join(FILE_NAME)).unwrap(), b"{ broken");
        fs::write(
            d.path().join(FILE_NAME),
            vec![b' '; MAX_FILE_BYTES as usize + 1],
        )
        .unwrap();
        assert_eq!(load(d.path()), Err(PresetError::TooLarge));
        fs::copy(
            fixtures().join("valid-basic.json"),
            d.path().join(FILE_NAME),
        )
        .unwrap();
        assert_eq!(load(d.path()).unwrap().len(), 2);
    }
}
