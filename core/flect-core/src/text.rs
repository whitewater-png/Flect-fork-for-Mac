//! Text tasks: encoding detection/conversion, line endings, JSON pretty/sort.
//! Pure functions over bytes/strings; file handling lives in the task layer.
use std::cmp::Ordering;
use std::fmt;

use encoding_rs::{Encoding, SHIFT_JIS, UTF_16BE, UTF_16LE, UTF_8};
use serde_json::{Map, Value};

use crate::settings::{JsonKeyOrder, JsonSortSettings, SjisFallback, TextEncodingChoice};

#[derive(Debug, PartialEq, Eq)]
pub enum TextError {
    /// A character cannot be represented and the policy is `Fail`.
    Unmappable(char),
    Json(String),
    Empty,
    /// A user-facing failure identified by message key (+ one argument).
    Msg(&'static str, String),
}

impl fmt::Display for TextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unmappable(c) => write!(f, "cannot represent {c:?}"),
            Self::Json(e) => write!(f, "invalid JSON: {e}"),
            Self::Empty => f.write_str("empty"),
            Self::Msg(k, a) => write!(f, "{k}: {a}"),
        }
    }
}

impl TextError {
    pub fn key(&self) -> &'static str {
        match self {
            Self::Unmappable(_) => "msg.sjisFailed",
            Self::Json(_) => "msg.invalidJson",
            Self::Empty => "msg.emptyFile",
            Self::Msg(k, _) => k,
        }
    }
}

/// The encoding a file was read with, so it can be written back unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detected {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
    Other(&'static Encoding),
}

pub struct Decoded {
    pub text: String,
    pub encoding: Detected,
}

/// BOM first, then strict UTF-8, then a statistical guess (Shift_JIS, etc.).
pub fn decode(bytes: &[u8]) -> Decoded {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return Decoded {
            text: UTF_8.decode_without_bom_handling(rest).0.into_owned(),
            encoding: Detected::Utf8Bom,
        };
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return Decoded {
            text: UTF_16LE.decode_without_bom_handling(rest).0.into_owned(),
            encoding: Detected::Utf16Le,
        };
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return Decoded {
            text: UTF_16BE.decode_without_bom_handling(rest).0.into_owned(),
            encoding: Detected::Utf16Be,
        };
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return Decoded {
            text: s.to_owned(),
            encoding: Detected::Utf8,
        };
    }
    let mut detector = chardetng::EncodingDetector::new();
    detector.feed(bytes, true);
    let guess = detector.guess(None, true);
    Decoded {
        text: guess.decode_without_bom_handling(bytes).0.into_owned(),
        encoding: Detected::Other(guess),
    }
}

pub struct Encoded {
    pub bytes: Vec<u8>,
    /// Some characters were replaced (only with `SjisFallback::ReplaceAndWarn`).
    pub replaced: bool,
}

fn utf16(text: &str, le: bool, bom: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() * 2 + 2);
    let push = |out: &mut Vec<u8>, u: u16| {
        out.extend_from_slice(&if le { u.to_le_bytes() } else { u.to_be_bytes() })
    };
    if bom {
        push(&mut out, 0xFEFF);
    }
    for u in text.encode_utf16() {
        push(&mut out, u);
    }
    out
}

/// Encode with a legacy charset; unmappable characters follow the fallback policy.
/// `encoding_rs` would emit HTML numeric references, so we detect and handle them ourselves.
fn encode_legacy(
    text: &str,
    enc: &'static Encoding,
    fallback: SjisFallback,
    substitute: &str,
) -> Result<Encoded, TextError> {
    let (bytes, _, unmappable) = enc.encode(text);
    if !unmappable {
        return Ok(Encoded {
            bytes: bytes.into_owned(),
            replaced: false,
        });
    }
    let replacement = match fallback {
        SjisFallback::Fail => {
            let bad = text
                .chars()
                .find(|c| enc.encode(c.encode_utf8(&mut [0; 4])).2)
                .unwrap_or('?');
            return Err(TextError::Unmappable(bad));
        }
        SjisFallback::Substitute => substitute,
        SjisFallback::ReplaceAndWarn => "?",
    };
    let mut out = Vec::with_capacity(text.len());
    let mut buf = [0u8; 4];
    for c in text.chars() {
        let (b, _, bad) = enc.encode(c.encode_utf8(&mut buf));
        if bad {
            out.extend_from_slice(&enc.encode(replacement).0);
        } else {
            out.extend_from_slice(&b);
        }
    }
    Ok(Encoded {
        bytes: out,
        replaced: fallback == SjisFallback::ReplaceAndWarn,
    })
}

pub fn encode(
    text: &str,
    target: TextEncodingChoice,
    fallback: SjisFallback,
    substitute: &str,
) -> Result<Encoded, TextError> {
    let plain = |bytes: Vec<u8>| {
        Ok(Encoded {
            bytes,
            replaced: false,
        })
    };
    match target {
        TextEncodingChoice::Utf8 => plain(text.as_bytes().to_vec()),
        TextEncodingChoice::Utf8Bom => plain([&[0xEF, 0xBB, 0xBF][..], text.as_bytes()].concat()),
        TextEncodingChoice::Utf16 => plain(utf16(text, true, true)),
        TextEncodingChoice::ShiftJis => encode_legacy(text, SHIFT_JIS, fallback, substitute),
    }
}

/// Write text back in the encoding it was read with.
pub fn encode_as_detected(text: &str, det: Detected) -> Vec<u8> {
    match det {
        Detected::Utf8 => text.as_bytes().to_vec(),
        Detected::Utf8Bom => [&[0xEF, 0xBB, 0xBF][..], text.as_bytes()].concat(),
        Detected::Utf16Le => utf16(text, true, true),
        Detected::Utf16Be => utf16(text, false, true),
        Detected::Other(enc) => encode_legacy(text, enc, SjisFallback::ReplaceAndWarn, "?")
            .map(|e| e.bytes)
            .unwrap_or_else(|_| text.as_bytes().to_vec()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    Crlf,
}

pub fn convert_line_endings(text: &str, to: LineEnding) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 16);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                push_ending(&mut out, to);
            }
            '\n' => push_ending(&mut out, to),
            c => out.push(c),
        }
    }
    out
}

fn push_ending(out: &mut String, to: LineEnding) {
    out.push_str(if to == LineEnding::Crlf { "\r\n" } else { "\n" });
}

// ---------------------------------------------------------------- JSON

/// Accept what the Windows tool accepted: `//` and `/* */` comments, trailing commas.
pub fn strip_jsonc(input: &str) -> String {
    let b = input.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    let mut in_str = false;
    while i < b.len() {
        let c = b[i];
        if in_str {
            out.push(c);
            if c == b'\\' && i + 1 < b.len() {
                out.push(b[i + 1]);
                i += 1;
            } else if c == b'"' {
                in_str = false;
            }
        } else if c == b'"' {
            in_str = true;
            out.push(c);
        } else if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
            continue;
        } else if c == b',' {
            let mut j = i + 1;
            while j < b.len() && b[j].is_ascii_whitespace() {
                j += 1;
            }
            if !matches!(b.get(j), Some(b'}' | b']')) {
                out.push(c);
            }
        } else {
            out.push(c);
        }
        i += 1;
    }
    String::from_utf8(out).unwrap_or_default()
}

pub fn parse_json(text: &str) -> Result<Value, TextError> {
    let text = text.trim_start_matches('\u{feff}');
    if text.trim().is_empty() {
        return Err(TextError::Empty);
    }
    serde_json::from_str(&strip_jsonc(text)).map_err(|e| TextError::Json(e.to_string()))
}

pub fn pretty_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

/// Natural ordering: digit runs compare by numeric value (ported from `NaturalComparer`).
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i].is_ascii_digit() && b[j].is_ascii_digit() {
            let (si, sj) = (i, j);
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let na: String = a[si..i]
                .iter()
                .collect::<String>()
                .trim_start_matches('0')
                .to_owned();
            let nb: String = b[sj..j]
                .iter()
                .collect::<String>()
                .trim_start_matches('0')
                .to_owned();
            let o = na.len().cmp(&nb.len()).then_with(|| na.cmp(&nb));
            if o != Ordering::Equal {
                return o;
            }
            continue;
        }
        let o = a[i].cmp(&b[j]);
        if o != Ordering::Equal {
            return o;
        }
        i += 1;
        j += 1;
    }
    (a.len() - i).cmp(&(b.len() - j))
}

fn compare(order: JsonKeyOrder, a: &str, b: &str) -> Ordering {
    match order {
        JsonKeyOrder::Ordinal => a.cmp(b),
        JsonKeyOrder::IgnoreCase => a.to_uppercase().cmp(&b.to_uppercase()),
        JsonKeyOrder::Natural => natural_cmp(a, b),
    }
}

fn arrange<T>(items: &mut [T], s: &JsonSortSettings, key: impl Fn(&T) -> String) {
    items.sort_by(|x, y| {
        let o = compare(s.comparison, &key(x), &key(y));
        if s.descending {
            o.reverse()
        } else {
            o
        }
    });
}

fn walk(node: &Value, s: &JsonSortSettings, pinned: &[String], depth: usize) -> Value {
    match node {
        Value::Object(map) => {
            if depth > 0 && !s.recursive {
                return node.clone();
            }
            let mut head: Vec<(&String, &Value)> = Vec::new();
            for key in pinned {
                if let Some(entry) = map.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)) {
                    if !head.iter().any(|(k, _)| *k == entry.0) {
                        head.push(entry);
                    }
                }
            }
            let mut tail: Vec<(&String, &Value)> = map
                .iter()
                .filter(|(k, _)| !head.iter().any(|(h, _)| h == k))
                .collect();
            arrange(&mut tail, s, |(k, _)| (*k).clone());
            let mut out = Map::new();
            for (k, v) in head.into_iter().chain(tail) {
                out.insert(k.clone(), walk(v, s, pinned, depth + 1));
            }
            Value::Object(out)
        }
        Value::Array(items) => {
            if depth > 0 && !s.recursive {
                return node.clone();
            }
            let mut items: Vec<Value> = items
                .iter()
                .map(|v| walk(v, s, pinned, depth + 1))
                .collect();
            if s.sort_primitive_arrays && items.iter().all(|v| !v.is_object() && !v.is_array()) {
                arrange(&mut items, s, |v| v.to_string());
            }
            Value::Array(items)
        }
        other => other.clone(),
    }
}

pub fn sort_json(value: &Value, s: &JsonSortSettings) -> Value {
    let pinned: Vec<String> = crate::folder::split_patterns(&s.pinned_keys);
    walk(value, s, &pinned, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_boms_and_utf8() {
        let d = decode(b"\xEF\xBB\xBFhi");
        assert_eq!((d.text.as_str(), d.encoding), ("hi", Detected::Utf8Bom));
        let d = decode(b"\xFF\xFEh\0i\0");
        assert_eq!((d.text.as_str(), d.encoding), ("hi", Detected::Utf16Le));
        let d = decode("日本語".as_bytes());
        assert_eq!((d.text.as_str(), d.encoding), ("日本語", Detected::Utf8));
    }

    #[test]
    fn detects_shift_jis_and_round_trips() {
        let sjis = encode(
            "これは日本語のテキストです。",
            TextEncodingChoice::ShiftJis,
            SjisFallback::Fail,
            "?",
        )
        .unwrap()
        .bytes;
        let d = decode(&sjis);
        assert_eq!(d.text, "これは日本語のテキストです。");
        assert!(matches!(d.encoding, Detected::Other(_)));
        assert_eq!(encode_as_detected(&d.text, d.encoding), sjis);
    }

    #[test]
    fn sjis_fallback_policies() {
        let t = "A😀B";
        assert_eq!(
            encode(t, TextEncodingChoice::ShiftJis, SjisFallback::Fail, "?").err(),
            Some(TextError::Unmappable('😀'))
        );
        let sub = encode(
            t,
            TextEncodingChoice::ShiftJis,
            SjisFallback::Substitute,
            "_",
        )
        .unwrap();
        assert_eq!((sub.bytes.as_slice(), sub.replaced), (&b"A_B"[..], false));
        let warn = encode(
            t,
            TextEncodingChoice::ShiftJis,
            SjisFallback::ReplaceAndWarn,
            "_",
        )
        .unwrap();
        assert_eq!((warn.bytes.as_slice(), warn.replaced), (&b"A?B"[..], true));
        // never numeric character references
        assert!(!String::from_utf8_lossy(&warn.bytes).contains("&#"));
    }

    #[test]
    fn utf16_has_bom_and_surrogates() {
        let b = encode("a😀", TextEncodingChoice::Utf16, SjisFallback::Fail, "")
            .unwrap()
            .bytes;
        assert_eq!(&b[..2], &[0xFF, 0xFE]);
        assert_eq!(decode(&b).text, "a😀");
    }

    #[test]
    fn line_endings() {
        assert_eq!(
            convert_line_endings("a\r\nb\rc\nd", LineEnding::Lf),
            "a\nb\nc\nd"
        );
        assert_eq!(
            convert_line_endings("a\r\nb\rc\nd\n", LineEnding::Crlf),
            "a\r\nb\r\nc\r\nd\r\n"
        );
        assert_eq!(convert_line_endings("a\n\n", LineEnding::Crlf), "a\r\n\r\n");
    }

    #[test]
    fn jsonc_input_is_accepted() {
        let v = parse_json("// c\n{ \"a\": [1, 2,], /* x */ \"u\": \"//not\\\"/*\", }").unwrap();
        assert_eq!(v["a"].as_array().unwrap().len(), 2);
        assert_eq!(v["u"], "//not\"/*");
        assert_eq!(parse_json("  ").err(), Some(TextError::Empty));
        assert!(matches!(parse_json("{"), Err(TextError::Json(_))));
    }

    #[test]
    fn pretty_keeps_order_numbers_and_unicode() {
        let v = parse_json(
            r#"{"z":1.0,"a":"日本","big":123456789012345678901234567890,"e":[],"o":{}}"#,
        )
        .unwrap();
        let p = pretty_json(&v);
        assert!(p.starts_with("{\n  \"z\": 1.0,"), "{p}");
        assert!(p.contains("123456789012345678901234567890"));
        assert!(p.contains("日本") && p.contains("\"e\": []") && p.contains("\"o\": {}"));
    }

    #[test]
    fn sort_rules() {
        let v =
            parse_json(r#"{"b":1,"B":2,"a":{"z":1,"y":2},"item10":0,"item2":0,"id":9}"#).unwrap();
        let keys = |v: &Value| v.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
        let mut s = JsonSortSettings::default();
        assert_eq!(
            keys(&sort_json(&v, &s)),
            ["B", "a", "b", "id", "item10", "item2"]
        );
        s.comparison = JsonKeyOrder::Natural;
        assert_eq!(
            keys(&sort_json(&v, &s)),
            ["B", "a", "b", "id", "item2", "item10"]
        );
        s.comparison = JsonKeyOrder::IgnoreCase;
        s.descending = true;
        assert_eq!(keys(&sort_json(&v, &s))[0], "item2");
        s.descending = false;
        s.pinned_keys = "id;a".into();
        assert_eq!(&keys(&sort_json(&v, &s))[..2], ["id", "a"]);
        // nested objects are sorted when recursive, left alone when not
        s.pinned_keys.clear();
        assert_eq!(keys(&sort_json(&v, &s)["a"]), ["y", "z"]);
        s.recursive = false;
        assert_eq!(keys(&sort_json(&v, &s)["a"]), ["z", "y"]);
    }

    #[test]
    fn primitive_arrays_only_when_enabled() {
        let v = parse_json("[3,1,2]").unwrap();
        let mut s = JsonSortSettings::default();
        assert_eq!(sort_json(&v, &s), v);
        s.sort_primitive_arrays = true;
        assert_eq!(sort_json(&v, &s), parse_json("[1,2,3]").unwrap());
    }

    #[test]
    fn natural_order() {
        assert_eq!(natural_cmp("a2", "a10"), Ordering::Less);
        assert_eq!(natural_cmp("a02", "a2"), Ordering::Equal);
        assert_eq!(natural_cmp("a", "ab"), Ordering::Less);
    }
}
