//! Output path derivation: results land next to the source and never overwrite
//! anything. A taken name gets a ` (2)`, ` (3)` ... suffix, like Finder does.
use std::path::{Path, PathBuf};

use crate::workspace::BRAND;

/// `true` for anything occupying the name, including dangling symlinks.
fn taken(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

fn parent_of(source: &Path) -> PathBuf {
    match source.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// `report.pdf` + `_ocr` + `.txt` -> `report_ocr.txt`, or `report_ocr (2).txt`.
pub fn derive(source: &Path, suffix: &str, extension: Option<&str>) -> PathBuf {
    let dir = parent_of(source);
    let stem = format!(
        "{}{}",
        source
            .file_stem()
            .map(|s| s.to_string_lossy())
            .unwrap_or_default(),
        suffix
    );
    let ext = match extension {
        Some(e) => normalize_ext(e),
        None => source
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default(),
    };
    let mut candidate = dir.join(format!("{stem}{ext}"));
    let mut n = 2u32;
    while taken(&candidate) {
        candidate = dir.join(format!("{stem} ({n}){ext}"));
        n += 1;
    }
    candidate
}

/// Same numbering for output folders.
pub fn derive_directory(source: &Path, stem: Option<&str>, suffix: &str) -> PathBuf {
    let dir = parent_of(source);
    let base = format!(
        "{}{}",
        stem.map(str::to_owned).unwrap_or_else(|| source
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()),
        suffix
    );
    let mut candidate = dir.join(&base);
    let mut n = 2u32;
    while taken(&candidate) {
        candidate = dir.join(format!("{base} ({n})"));
        n += 1;
    }
    candidate
}

/// Hidden sibling used while a file is being written. Unique per call.
pub fn scratch(final_path: &Path, extension: &str) -> PathBuf {
    parent_of(final_path).join(format!(
        ".{BRAND}-{}{}",
        crate::workspace::unique_token(),
        normalize_ext(extension)
    ))
}

fn normalize_ext(ext: &str) -> String {
    match ext {
        "" => String::new(),
        e if e.starts_with('.') => e.to_owned(),
        e => format!(".{e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_taken_names() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.png");
        std::fs::write(&src, b"x").unwrap();
        assert_eq!(derive(&src, "_s", Some("jpg")), dir.path().join("a_s.jpg"));
        std::fs::write(dir.path().join("a_s.jpg"), b"x").unwrap();
        assert_eq!(
            derive(&src, "_s", Some(".jpg")),
            dir.path().join("a_s (2).jpg")
        );
        std::fs::write(dir.path().join("a_s (2).jpg"), b"x").unwrap();
        assert_eq!(
            derive(&src, "_s", Some("jpg")),
            dir.path().join("a_s (3).jpg")
        );
    }

    #[test]
    fn keeps_source_extension_and_unicode() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("写真 -1.PNG");
        assert_eq!(derive(&src, "", None), dir.path().join("写真 -1.PNG"));
        std::fs::write(&src, b"x").unwrap();
        assert_eq!(derive(&src, "", None), dir.path().join("写真 -1 (2).PNG"));
    }

    #[test]
    fn dangling_symlink_counts_as_taken() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.txt");
        std::os::unix::fs::symlink(dir.path().join("nowhere"), dir.path().join("a_x.txt")).unwrap();
        assert_eq!(derive(&src, "_x", None), dir.path().join("a_x (2).txt"));
    }

    #[test]
    fn directories_are_numbered() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("pack.zip");
        std::fs::create_dir(dir.path().join("pack")).unwrap();
        assert_eq!(
            derive_directory(&src, None, ""),
            dir.path().join("pack (2)")
        );
    }

    #[test]
    fn scratch_is_hidden_and_unique() {
        let f = Path::new("/tmp/out.mp4");
        let (a, b) = (scratch(f, "mp4"), scratch(f, ".mp4"));
        assert_ne!(a, b);
        assert!(a
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(".Flect-for-Mac-"));
        assert!(a.to_string_lossy().ends_with(".mp4"));
    }
}
