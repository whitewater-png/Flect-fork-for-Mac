//! Menu catalog (single source of truth, ported from the Windows `MenuCatalog.cs`)
//! and selection classification (ported from `Selection.cpp`).
//! Exported as `menu.json` for the Swift side; labels are keys plus an English default.
use std::path::Path;

use serde::Serialize;
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize)]
pub struct MenuEntry {
    pub id: String,
    pub label_key: String,
    pub label: String,
    pub children: Vec<MenuEntry>,
}

fn leaf(id: &str, label: &str) -> MenuEntry {
    leaf_key(id, label, &format!("menu.{id}"))
}
fn leaf_key(id: &str, label: &str, key: &str) -> MenuEntry {
    MenuEntry {
        id: id.into(),
        label_key: key.into(),
        label: label.into(),
        children: vec![],
    }
}
fn group(id: &str, label: &str, children: Vec<MenuEntry>) -> MenuEntry {
    MenuEntry {
        id: id.into(),
        label_key: format!("menu.{id}"),
        label: label.into(),
        children,
    }
}
fn preset(id: &str, n: u8) -> MenuEntry {
    leaf_key(id, &format!("Preset {n}"), &format!("menu.preset{n}"))
}
fn custom(id: &str, label: &str) -> MenuEntry {
    leaf_key(id, label, "menu.custom")
}
fn to_all(prefix: &str, formats: &[&str]) -> Vec<MenuEntry> {
    formats
        .iter()
        .map(|f| leaf_key(&format!("{prefix}.{f}"), &format!("to {f}"), "menu.to"))
        .collect()
}

const AUDIO: &[&str] = &["MP3", "M4A", "WAV", "AIF", "AIFF", "AAC", "OGG", "WMA"];

pub fn catalog() -> Vec<MenuEntry> {
    vec![
        group(
            "folder",
            "Folder",
            vec![
                leaf("folder.removeDuplicate", "Remove Duplicate"),
                leaf("folder.removeEmpty", "Remove Empty"),
                leaf("folder.tree", "Tree"),
                leaf("folder.rename", "Rename"),
                leaf("folder.analyze", "Analyze"),
                leaf("folder.compress", "Compress"),
            ],
        ),
        group(
            "archive",
            "Archive",
            vec![leaf("archive.extract", "Extract")],
        ),
        group(
            "pdf",
            "PDF",
            vec![
                leaf("pdf.preview", "Preview"),
                leaf("pdf.merge", "Merge"),
                leaf_key("pdf.convert.PNG", "to PNG", "menu.to"),
                leaf_key("pdf.convert.JPG", "to JPG", "menu.to"),
                leaf_key("pdf.convert.TXT", "to TXT", "menu.to"),
            ],
        ),
        group(
            "image",
            "Image",
            vec![
                group(
                    "image.resize",
                    "Resize",
                    vec![
                        preset("image.resize.p1", 1),
                        preset("image.resize.p2", 2),
                        custom("image.resize.custom", "Custom..."),
                    ],
                ),
                group(
                    "image.compress",
                    "Compress",
                    vec![
                        preset("image.compress.p1", 1),
                        preset("image.compress.p2", 2),
                        preset("image.compress.p3", 3),
                        preset("image.compress.p4", 4),
                        custom("image.compress.custom", "Custom..."),
                    ],
                ),
                group(
                    "image.rotate",
                    "Rotate",
                    vec![
                        preset("image.rotate.p1", 1),
                        preset("image.rotate.p2", 2),
                        preset("image.rotate.p3", 3),
                    ],
                ),
                group(
                    "image.convert",
                    "Convert",
                    to_all(
                        "image.convert",
                        &["PNG", "JPG", "WEBP", "HEIC", "ICO", "GIF", "SVG"],
                    ),
                ),
                leaf("image.ocr", "OCR"),
                leaf("image.strip", "Remove Metadata"),
                leaf("image.privacy", "Privacy Check"),
                leaf("image.makeSafe", "Make Safe to Share"),
                leaf("image.makeSafe.resize", "Make Safe to Share (Resize)"),
                leaf("image.chromakey", "Remove Background"),
            ],
        ),
        group(
            "video",
            "Video",
            vec![
                group(
                    "video.resize",
                    "Resize",
                    vec![
                        preset("video.resize.p1", 1),
                        preset("video.resize.p2", 2),
                        custom("video.resize.custom", "Custom"),
                    ],
                ),
                leaf("video.trim", "Trim"),
                leaf("video.thumbnail", "Thumbnail"),
                leaf("video.subtitles", "Subtitles"),
                group(
                    "video.compress",
                    "Compress",
                    vec![
                        leaf("video.compress.discord", "Discord"),
                        preset("video.compress.p1", 1),
                        preset("video.compress.p2", 2),
                        preset("video.compress.p3", 3),
                        custom("video.compress.custom", "Custom"),
                    ],
                ),
                group(
                    "video.rotate",
                    "Rotate",
                    vec![
                        preset("video.rotate.p1", 1),
                        preset("video.rotate.p2", 2),
                        preset("video.rotate.p3", 3),
                    ],
                ),
                group(
                    "video.convert",
                    "Convert",
                    to_all(
                        "video.convert",
                        &["MP4", "MOV", "MKV", "M4A", "AVI", "WEBM", "FLV", "GIF"],
                    ),
                ),
                group(
                    "video.extractAudio",
                    "Extract Audio",
                    to_all("video.extractAudio", AUDIO),
                ),
            ],
        ),
        group(
            "audio",
            "Audio",
            vec![
                group("audio.convert", "Convert", to_all("audio.convert", AUDIO)),
                leaf("audio.subtitles", "Subtitles"),
            ],
        ),
        group(
            "text",
            "Text",
            vec![
                group(
                    "text.convert",
                    "Convert",
                    to_all("text.convert", &["TXT", "JSON", "XML", "CSV", "HTML", "MD"]),
                ),
                group(
                    "text.encode",
                    "Format",
                    vec![
                        leaf_key("text.encode.UTF8", "to UTF-8", "menu.to"),
                        leaf_key("text.encode.UTF8BOM", "to UTF-8 (BOM)", "menu.to"),
                        leaf_key("text.encode.UTF16", "to UTF-16", "menu.to"),
                        leaf_key("text.encode.SJIS", "to Shift_JIS", "menu.to"),
                    ],
                ),
                group(
                    "text.lineEndings",
                    "Line Endings",
                    vec![
                        leaf_key("text.lineEndings.CRLF", "to CRLF", "menu.to"),
                        leaf_key("text.lineEndings.LF", "to LF (Unix)", "menu.to"),
                    ],
                ),
                leaf("json.pretty", "Pretty Print"),
                leaf("json.sort", "Sort Keys"),
            ],
        ),
        group("jar", "JAR", vec![leaf("jar.decompile", "Decompile")]),
    ]
}

pub fn leaf_ids(entries: &[MenuEntry]) -> Vec<&str> {
    let mut out = Vec::new();
    for e in entries {
        if e.children.is_empty() {
            out.push(e.id.as_str());
        } else {
            out.extend(leaf_ids(&e.children));
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    Folder,
    Pdf,
    Image,
    Video,
    Audio,
    Text,
    Jar,
    Archive,
}

const IMAGE: &[(&str, &str)] = &[
    ("png", "PNG"),
    ("jpg", "JPG"),
    ("jpeg", "JPG"),
    ("webp", "WEBP"),
    ("heic", "HEIC"),
    ("heif", "HEIC"),
    ("ico", "ICO"),
    ("gif", "GIF"),
    ("svg", "SVG"),
];
const VIDEO: &[(&str, &str)] = &[
    ("mp4", "MP4"),
    ("mov", "MOV"),
    ("mkv", "MKV"),
    ("m4a", "M4A"),
    ("avi", "AVI"),
    ("webm", "WEBM"),
    ("flv", "FLV"),
];
const AUDIO_EXT: &[(&str, &str)] = &[
    ("mp3", "MP3"),
    ("wav", "WAV"),
    ("aif", "AIF"),
    ("aiff", "AIFF"),
    ("aac", "AAC"),
    ("ogg", "OGG"),
    ("wma", "WMA"),
];
const TEXT_FORMATS: &[(&str, &str)] = &[
    ("txt", "TXT"),
    ("text", "TXT"),
    ("json", "JSON"),
    ("csv", "CSV"),
    ("xml", "XML"),
    ("ini", "INI"),
    ("cfg", "CFG"),
    ("conf", "CFG"),
    ("css", "CSS"),
    ("html", "HTML"),
    ("htm", "HTML"),
    ("md", "MD"),
    ("markdown", "MD"),
];
const TEXT_EXT: &[&str] = &[
    "txt",
    "text",
    "md",
    "markdown",
    "log",
    "csv",
    "tsv",
    "ini",
    "cfg",
    "conf",
    "json",
    "xml",
    "yaml",
    "yml",
    "toml",
    "html",
    "htm",
    "css",
    "js",
    "mjs",
    "cjs",
    "ts",
    "tsx",
    "jsx",
    "c",
    "h",
    "cpp",
    "hpp",
    "cc",
    "hh",
    "cs",
    "java",
    "kt",
    "py",
    "rb",
    "go",
    "rs",
    "php",
    "pl",
    "lua",
    "sh",
    "bash",
    "bat",
    "cmd",
    "ps1",
    "psm1",
    "sql",
    "srt",
    "vtt",
    "asc",
    "tex",
    "rst",
    "env",
    "props",
    "properties",
    "gradle",
    "cmake",
    "m",
    "mm",
    "swift",
    "vb",
    "r",
    "jl",
    "zsh",
];

#[derive(Debug, PartialEq, Eq)]
pub struct Classification {
    pub category: Category,
    pub format: Option<&'static str>,
}

fn lookup(table: &[(&str, &'static str)], ext: &str) -> Option<&'static str> {
    table.iter().find(|(e, _)| *e == ext).map(|(_, f)| *f)
}

/// Classify one selected item (mirrors `Selection.cpp::Classify`).
pub fn classify(path: &Path, is_dir: bool) -> Option<Classification> {
    let c = |category, format| Some(Classification { category, format });
    if is_dir {
        return c(Category::Folder, None);
    }
    let name = path.file_name()?.to_string_lossy().to_lowercase();
    if name.len() > 7 && name.ends_with(".tar.gz") {
        return c(Category::Archive, Some("TAR.GZ"));
    }
    let ext = path.extension()?.to_string_lossy().to_lowercase();
    match ext.as_str() {
        "pdf" => return c(Category::Pdf, Some("PDF")),
        "jar" => return c(Category::Jar, Some("JAR")),
        "zip" => return c(Category::Archive, Some("ZIP")),
        "7z" => return c(Category::Archive, Some("7Z")),
        "tar" => return c(Category::Archive, Some("TAR")),
        "rar" => return c(Category::Archive, Some("RAR")),
        _ => {}
    }
    if let Some(f) = lookup(IMAGE, &ext) {
        return c(Category::Image, Some(f));
    }
    if let Some(f) = lookup(VIDEO, &ext) {
        return c(Category::Video, Some(f));
    }
    if let Some(f) = lookup(AUDIO_EXT, &ext) {
        return c(Category::Audio, Some(f));
    }
    if TEXT_EXT.contains(&ext.as_str()) {
        return c(
            Category::Text,
            Some(lookup(TEXT_FORMATS, &ext).unwrap_or("TEXT")),
        );
    }
    None
}

/// Text conversion targets per source format (mirrors `TextTargets`).
pub fn text_targets(from: &str) -> Vec<&'static str> {
    let mut t = Vec::new();
    if from != "TXT" {
        t.push("TXT");
    }
    match from {
        "CSV" => t.extend(["JSON", "XML", "HTML", "MD"]),
        "JSON" => t.extend(["CSV", "XML"]),
        "XML" => t.push("JSON"),
        "INI" | "CFG" => t.extend(["JSON", "XML"]),
        _ => {}
    }
    t
}

fn table(rows: &[(&str, &str)]) -> Value {
    Value::Array(
        rows.iter()
            .map(|(e, f)| json!({"extension": e, "format": f}))
            .collect(),
    )
}

/// The `menu.json` document consumed by the Swift app and the Quick Action generator.
pub fn export() -> Value {
    json!({
        "version": 1,
        "entries": catalog(),
        "selection": {
            "image": table(IMAGE),
            "video": table(VIDEO),
            "audio": table(AUDIO_EXT),
            "text": table(TEXT_FORMATS),
            "textExtensions": TEXT_EXT,
            "archive": ["zip", "7z", "tar", "tar.gz", "rar"],
            "other": {"pdf": ["pdf"], "jar": ["jar"]},
        },
        "rules": {
            "pdf.preview": "single",
            "pdf.merge": "multiple",
            "video.trim": "single",
            "video.thumbnail": "single",
            "image.convert": "not-animated-gif",
            "json.pretty": "all-json",
            "json.sort": "all-json",
            "text.convert": "single-format",
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn catalog_matches_windows_leaf_count() {
        // legacy/windows/.../MenuCatalog.cs declares 87 leaf ids; the Mac port adds 3 (privacy check, make safe, make safe + resize).
        assert_eq!(leaf_ids(&catalog()).len(), 90);
    }

    #[test]
    fn ids_are_unique() {
        let catalog = catalog();
        let ids = leaf_ids(&catalog);
        assert_eq!(ids.len(), ids.iter().collect::<HashSet<_>>().len());
    }

    #[test]
    fn classification() {
        let k = |p: &str| classify(Path::new(p), false);
        assert_eq!(
            k("/a/b.JPEG").unwrap(),
            Classification {
                category: Category::Image,
                format: Some("JPG")
            }
        );
        assert_eq!(k("/a/b.tar.gz").unwrap().format, Some("TAR.GZ"));
        assert_eq!(k("/a/b.rs").unwrap().format, Some("TEXT"));
        assert_eq!(k("/a/b.mov").unwrap().category, Category::Video);
        assert!(k("/a/noext").is_none());
        assert!(k("/a/b.exe").is_none());
        assert_eq!(
            classify(Path::new("/a"), true).unwrap().category,
            Category::Folder
        );
    }

    #[test]
    fn text_targets_follow_source() {
        assert_eq!(text_targets("CSV"), ["TXT", "JSON", "XML", "HTML", "MD"]);
        assert_eq!(text_targets("TXT"), Vec::<&str>::new());
    }

    #[test]
    fn export_is_serializable() {
        let v = export();
        assert_eq!(v["version"], 1);
        assert!(v["entries"].as_array().unwrap().len() == 8);
    }
}
