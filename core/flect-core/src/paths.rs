//! Output path derivation: results land next to the source (or in the folder a
//! setting names) and never overwrite anything. A taken name gets a ` (2)`,
//! ` (3)` ... suffix, like Finder does.
use std::io;
use std::path::{Component, Path, PathBuf};

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

/// Where results go for `source`, given the "output folder" setting of its feature.
/// Empty: beside the source. `~` / `~/x`: the home folder. Relative (`./Compressed`,
/// `../out`): from the source's folder. Absolute: as written. `.` and `..` are resolved
/// textually and never climb above the root. Pure: nothing is created here.
pub fn output_dir(source: &Path, setting: &str) -> PathBuf {
    let setting = setting.trim();
    if setting.is_empty() {
        return parent_of(source);
    }
    let joined = match setting.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => match home_dir() {
            Some(home) => home.join(rest.trim_start_matches('/')),
            None => parent_of(source).join(setting),
        },
        _ if Path::new(setting).is_absolute() => PathBuf::from(setting),
        _ => parent_of(source).join(setting),
    };
    lexical_clean(&joined)
}

/// [`output_dir`], then make sure the folder exists (a file in the way is an error).
pub fn prepare_output_dir(source: &Path, setting: &str) -> io::Result<PathBuf> {
    let dir = output_dir(source, setting);
    if !setting.trim().is_empty() {
        std::fs::create_dir_all(&dir)?;
    }
    Ok(dir)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|h| h.is_absolute())
}

/// Drop `.` and fold `..` into the component before it. A leading `..` with nothing to
/// cancel (relative path) is kept; at the root it is dropped.
fn lexical_clean(path: &Path) -> PathBuf {
    let mut out: Vec<Component> = Vec::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => match out.last() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir | Component::Prefix(_)) => {}
                _ => out.push(c),
            },
            _ => out.push(c),
        }
    }
    if out.is_empty() {
        return PathBuf::from(".");
    }
    out.iter().collect()
}

/// `report.pdf` + `_ocr` + `.txt` -> `report_ocr.txt`, or `report_ocr (2).txt`.
pub fn derive(source: &Path, suffix: &str, extension: Option<&str>) -> PathBuf {
    derive_in(&parent_of(source), source, suffix, extension)
}

/// Same as [`derive`], but in `dir`.
pub fn derive_in(dir: &Path, source: &Path, suffix: &str, extension: Option<&str>) -> PathBuf {
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
    derive_directory_in(&parent_of(source), source, stem, suffix)
}

/// Same as [`derive_directory`], but inside `dir`.
pub fn derive_directory_in(dir: &Path, source: &Path, stem: Option<&str>, suffix: &str) -> PathBuf {
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
    fn unset_folder_means_beside_the_source() {
        let src = Path::new("/data/photos/a.png");
        assert_eq!(output_dir(src, ""), Path::new("/data/photos"));
        assert_eq!(output_dir(src, "   "), Path::new("/data/photos"));
        assert_eq!(output_dir(Path::new("a.png"), ""), Path::new("."));
    }

    #[test]
    fn relative_folders_start_from_the_source_folder() {
        let src = Path::new("/data/photos/a.png");
        assert_eq!(
            output_dir(src, "./Compressed"),
            Path::new("/data/photos/Compressed")
        );
        assert_eq!(
            output_dir(src, "Compressed/small"),
            Path::new("/data/photos/Compressed/small")
        );
        assert_eq!(output_dir(src, "../out"), Path::new("/data/out"));
        assert_eq!(output_dir(src, "./a/../b/."), Path::new("/data/photos/b"));
        assert_eq!(output_dir(src, "../../../../x"), Path::new("/x"));
    }

    #[test]
    fn absolute_and_home_folders() {
        let src = Path::new("/data/photos/a.png");
        assert_eq!(
            output_dir(src, "/srv/out/../results"),
            Path::new("/srv/results")
        );
        assert_eq!(output_dir(src, " /srv/out "), Path::new("/srv/out"));
        let home = PathBuf::from(std::env::var_os("HOME").expect("HOME is set"));
        assert_eq!(output_dir(src, "~"), home);
        assert_eq!(output_dir(src, "~/Flect/out"), home.join("Flect/out"));
        // `~name` is not expanded: it is an ordinary relative folder.
        assert_eq!(output_dir(src, "~other"), Path::new("/data/photos/~other"));
    }

    #[test]
    fn folder_is_created_and_names_stay_unique() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.png");
        std::fs::write(&src, b"x").unwrap();
        let out = prepare_output_dir(&src, "./Compressed/deep").unwrap();
        assert_eq!(out, dir.path().join("Compressed/deep"));
        assert!(out.is_dir());
        assert_eq!(
            derive_in(&out, &src, "_c", Some("jpg")),
            out.join("a_c.jpg")
        );
        std::fs::write(out.join("a_c.jpg"), b"x").unwrap();
        assert_eq!(
            derive_in(&out, &src, "_c", Some("jpg")),
            out.join("a_c (2).jpg")
        );
        // Unset: nothing is created and the source's folder is returned.
        assert_eq!(prepare_output_dir(&src, "").unwrap(), dir.path());
    }

    #[test]
    fn a_file_in_the_way_is_an_error_not_an_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.png");
        std::fs::write(dir.path().join("Out"), b"keep").unwrap();
        assert!(prepare_output_dir(&src, "Out").is_err());
        assert_eq!(std::fs::read(dir.path().join("Out")).unwrap(), b"keep");
    }

    #[test]
    fn directories_are_numbered_in_a_chosen_folder() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("pack.zip");
        let out = dir.path().join("Extracted");
        std::fs::create_dir_all(out.join("pack")).unwrap();
        assert_eq!(
            derive_directory_in(&out, &src, Some("pack"), ""),
            out.join("pack (2)")
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
