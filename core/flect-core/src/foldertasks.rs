//! Folder operations that need a plan before touching anything: rename, analyze,
//! and the delete plans for duplicates / empty folders. Deleting itself is either done
//! here (permanent mode) or requested from the app as `needs{trash}` so it lands in the Trash.
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::folder::{self, Cancelled, ExclusionFilter, Progress, ScannedFile, TreeScan};
use crate::settings::{FolderSettings, RenameSettings, ReportFormat};
use crate::text::natural_cmp;

// ------------------------------------------------------------------ rename

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenamePlan {
    pub prefix: String,
    pub name: String,
    pub suffix: String,
    pub numbered: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RenameError {
    /// Empty result, or contains a path separator / NUL.
    InvalidName,
    Io(String),
}

impl RenamePlan {
    /// The app validates this too; the engine never trusts the UI.
    pub fn validate(&self) -> Result<(), RenameError> {
        let probe = format!(
            "{}{}{}",
            self.prefix,
            if self.numbered { "1" } else { &self.name },
            self.suffix
        );
        if probe.trim().is_empty()
            || probe.contains(['/', '\0'])
            || probe == "."
            || probe == ".."
            || probe.len() > 200
        {
            return Err(RenameError::InvalidName);
        }
        Ok(())
    }
}

/// Finder-like order: case-insensitive, numbers by value.
pub fn explorer_order(a: &str, b: &str) -> std::cmp::Ordering {
    natural_cmp(&a.to_lowercase(), &b.to_lowercase()).then_with(|| a.cmp(b))
}

#[derive(Debug, PartialEq, Eq)]
pub struct Rename {
    pub from: PathBuf,
    pub to: PathBuf,
}

/// Plan the renames of one folder: files are grouped by extension (case-insensitive),
/// ordered naturally, and numbered independently per group.
pub fn plan_rename(
    folder: &Path,
    plan: &RenamePlan,
    s: &RenameSettings,
    excl: &ExclusionFilter,
) -> io::Result<Vec<Rename>> {
    let mut groups: HashMap<String, Vec<(String, PathBuf, String)>> = HashMap::new();
    for e in fs::read_dir(folder)?.flatten() {
        let Ok(meta) = e.path().symlink_metadata() else {
            continue;
        };
        let name = e.file_name().to_string_lossy().into_owned();
        if !meta.is_file() || excl.excludes_file(&name) {
            continue;
        }
        let ext = Path::new(&name)
            .extension()
            .map(|x| format!(".{}", x.to_string_lossy()))
            .unwrap_or_default();
        groups
            .entry(ext.to_lowercase())
            .or_default()
            .push((name, e.path(), ext));
    }
    let mut keys: Vec<_> = groups.keys().cloned().collect();
    keys.sort();
    let mut out = Vec::new();
    for key in keys {
        let mut files = groups.remove(&key).unwrap_or_default();
        files.sort_by(|a, b| explorer_order(&a.0, &b.0));
        let highest = i64::from(s.start_number) + files.len() as i64 - 1;
        let width = if s.digit_count > 0 {
            s.digit_count as usize
        } else {
            highest.max(1).to_string().len()
        };
        let mut taken: HashSet<String> = HashSet::new();
        for (i, (_, path, ext)) in files.iter().enumerate() {
            let middle = if plan.numbered {
                format!("{:0>width$}", i64::from(s.start_number) + i as i64)
            } else {
                plan.name.clone()
            };
            let stem = format!("{}{}{}", plan.prefix, middle, plan.suffix);
            let mut name = format!("{stem}{ext}");
            let mut n = 2;
            while !taken.insert(name.to_lowercase()) {
                name = format!("{stem} ({n}){ext}");
                n += 1;
            }
            out.push(Rename {
                from: path.clone(),
                to: folder.join(name),
            });
        }
    }
    Ok(out)
}

#[derive(Debug, Default)]
pub struct RenameOutcome {
    pub renamed: usize,
    pub errors: Vec<(String, String)>,
}

/// Apply in two phases (everything to a temporary name first) so swaps and shifted
/// numbering cannot clobber a file that has not been moved yet. Never overwrites.
pub fn apply_rename(
    plan: &[Rename],
    cancelled: &dyn Fn() -> bool,
) -> Result<RenameOutcome, Cancelled> {
    let mut out = RenameOutcome::default();
    let mut staged: Vec<(PathBuf, &Rename)> = Vec::new();
    for r in plan {
        if cancelled() {
            roll_back(&staged);
            return Err(Cancelled);
        }
        if r.from == r.to {
            continue;
        }
        let tmp = crate::paths::scratch(
            &r.to,
            r.from
                .extension()
                .map(|e| e.to_string_lossy().into_owned())
                .as_deref()
                .unwrap_or(""),
        );
        match fs::rename(&r.from, &tmp) {
            Ok(()) => staged.push((tmp, r)),
            Err(e) => out.errors.push((name_of(&r.from), e.to_string())),
        }
    }
    for (tmp, r) in staged {
        // hard_link fails on an existing target, which rename would silently replace
        let moved = fs::hard_link(&tmp, &r.to).and_then(|()| fs::remove_file(&tmp));
        match moved {
            Ok(()) => out.renamed += 1,
            Err(e) => {
                out.errors.push((name_of(&r.to), e.to_string()));
                if fs::rename(&tmp, &r.from).is_err() {
                    out.errors
                        .push((name_of(&tmp), "left behind under a temporary name".into()));
                }
            }
        }
    }
    Ok(out)
}

fn roll_back(staged: &[(PathBuf, &Rename)]) {
    for (tmp, r) in staged {
        let _ = fs::rename(tmp, &r.from);
    }
}

fn name_of(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub fn dry_run_lines(plan: &[Rename]) -> Vec<String> {
    plan.iter()
        .map(|r| format!("{}  ->  {}", name_of(&r.from), name_of(&r.to)))
        .collect()
}

// ------------------------------------------------------------------ analyze

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeShare {
    pub extension: String,
    pub count: usize,
    pub bytes: u64,
}

#[derive(Debug)]
pub struct AnalyzeReport {
    pub roots: Vec<PathBuf>,
    pub total_bytes: u64,
    pub file_count: usize,
    pub directory_count: usize,
    pub largest: Vec<ScannedFile>,
    pub by_type: Vec<TypeShare>,
    pub duplicate_groups: usize,
    pub duplicate_files: usize,
    pub reclaimable_bytes: u64,
    pub empty_directories: Vec<PathBuf>,
    pub elapsed: Duration,
}

pub const TOP_COUNT: usize = 15;

pub fn build_report(
    roots: &[PathBuf],
    scan: &TreeScan,
    duplicates: &[Vec<ScannedFile>],
    elapsed: Duration,
) -> AnalyzeReport {
    let mut by: HashMap<String, (usize, u64)> = HashMap::new();
    for f in &scan.files {
        let key = if f.extension.is_empty() {
            "(no extension)".to_owned()
        } else {
            f.extension.clone()
        };
        let e = by.entry(key).or_default();
        e.0 += 1;
        e.1 += f.size;
    }
    let mut by_type: Vec<TypeShare> = by
        .into_iter()
        .map(|(extension, (count, bytes))| TypeShare {
            extension,
            count,
            bytes,
        })
        .collect();
    by_type.sort_by(|a, b| {
        b.bytes
            .cmp(&a.bytes)
            .then_with(|| a.extension.cmp(&b.extension))
    });
    let mut largest = scan.files.clone();
    largest.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.path.cmp(&b.path)));
    largest.truncate(TOP_COUNT);
    AnalyzeReport {
        roots: roots.to_vec(),
        total_bytes: scan.total_bytes(),
        file_count: scan.files.len(),
        directory_count: scan.directory_count,
        largest,
        by_type,
        duplicate_groups: duplicates.len(),
        duplicate_files: duplicates.iter().map(Vec::len).sum(),
        reclaimable_bytes: duplicates
            .iter()
            .map(|g| g.iter().skip(1).map(|f| f.size).sum::<u64>())
            .sum(),
        empty_directories: scan.empty_directories.clone(),
        elapsed,
    }
}

pub fn report_extension(f: ReportFormat) -> &'static str {
    match f {
        ReportFormat::Json => ".json",
        ReportFormat::Html => ".html",
        ReportFormat::Txt => ".txt",
        ReportFormat::Csv => ".csv",
    }
}

fn csv_q(v: &str) -> String {
    if v.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", v.replace('"', "\"\""))
    } else {
        v.to_owned()
    }
}

fn html_e(v: &str) -> String {
    v.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn p(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Render the report. Text-ish formats carry raw numbers; formatting for humans is the UI's job
/// (the HTML/TXT variants use plain byte counts so the file is locale-independent).
pub fn render_report(r: &AnalyzeReport, format: ReportFormat) -> Vec<u8> {
    let total = r.total_bytes.max(1) as f64;
    match format {
        ReportFormat::Csv => {
            let mut s = String::from("\u{feff}section,name,bytes,count,share\n");
            let mut row = |sec: &str,
                           name: &str,
                           bytes: Option<u64>,
                           count: Option<usize>,
                           share: Option<f64>| {
                s.push_str(&format!(
                    "{},{},{},{},{}\n",
                    csv_q(sec),
                    csv_q(name),
                    bytes.map(|v| v.to_string()).unwrap_or_default(),
                    count.map(|v| v.to_string()).unwrap_or_default(),
                    share.map(|v| format!("{v:.5}")).unwrap_or_default()
                ));
            };
            for root in &r.roots {
                row("root", &p(root), None, None, None);
            }
            row(
                "summary",
                "total",
                Some(r.total_bytes),
                Some(r.file_count),
                None,
            );
            row("summary", "folders", None, Some(r.directory_count), None);
            row(
                "summary",
                "duplicateGroups",
                Some(r.reclaimable_bytes),
                Some(r.duplicate_groups),
                None,
            );
            row(
                "summary",
                "duplicateFiles",
                None,
                Some(r.duplicate_files),
                None,
            );
            row(
                "summary",
                "emptyFolders",
                None,
                Some(r.empty_directories.len()),
                None,
            );
            for f in &r.largest {
                row("largest", &p(&f.path), Some(f.size), None, None);
            }
            for t in &r.by_type {
                row(
                    "type",
                    &t.extension,
                    Some(t.bytes),
                    Some(t.count),
                    Some(t.bytes as f64 / total),
                );
            }
            for d in &r.empty_directories {
                row("empty", &p(d), None, None, None);
            }
            s.into_bytes()
        }
        ReportFormat::Json => {
            let v = serde_json::json!({
                "roots": r.roots.iter().map(|x| p(x)).collect::<Vec<_>>(),
                "totalBytes": r.total_bytes,
                "files": r.file_count,
                "folders": r.directory_count,
                "scanSeconds": (r.elapsed.as_secs_f64() * 100.0).round() / 100.0,
                "duplicates": {"groups": r.duplicate_groups, "files": r.duplicate_files, "reclaimableBytes": r.reclaimable_bytes},
                "largest": r.largest.iter().map(|f| serde_json::json!({"path": p(&f.path), "bytes": f.size})).collect::<Vec<_>>(),
                "byType": r.by_type.iter().map(|t| serde_json::json!({"extension": t.extension, "files": t.count, "bytes": t.bytes})).collect::<Vec<_>>(),
                "emptyFolders": r.empty_directories.iter().map(|x| p(x)).collect::<Vec<_>>(),
            });
            serde_json::to_vec_pretty(&v).unwrap_or_default()
        }
        ReportFormat::Txt => {
            let mut s = format!("{}\n\n{} bytes - {} files - {} folders\n{} duplicate groups - {} bytes reclaimable\n\n", r.roots.iter().map(|x| p(x)).collect::<Vec<_>>().join("   "), r.total_bytes, r.file_count, r.directory_count, r.duplicate_groups, r.reclaimable_bytes);
            for f in &r.largest {
                s.push_str(&format!("{:>14}  {}\n", f.size, p(&f.path)));
            }
            s.push('\n');
            for t in &r.by_type {
                s.push_str(&format!(
                    "{:<16}{:>14}  {:>6.1}%  {} files\n",
                    t.extension,
                    t.bytes,
                    t.bytes as f64 / total * 100.0,
                    t.count
                ));
            }
            s.push('\n');
            for d in &r.empty_directories {
                s.push_str(&p(d));
                s.push('\n');
            }
            s.into_bytes()
        }
        ReportFormat::Html => {
            let mut s = String::from("<!doctype html>\n<html><head><meta charset=\"utf-8\"><title>Flect-for-Mac - Analyze</title>\n<style>body{font-family:-apple-system,system-ui,sans-serif;margin:32px;color:#1b1b1b}h2{font-size:15px;margin:26px 0 8px}table{border-collapse:collapse;width:100%;font-size:13px}td,th{text-align:left;padding:4px 10px;border-bottom:1px solid #e4e4e4}td.n{text-align:right;font-variant-numeric:tabular-nums}@media(prefers-color-scheme:dark){body{background:#1f1f1f;color:#f2f2f2}td,th{border-bottom-color:#3d3d3d}}</style>\n</head><body>\n");
            s.push_str(&format!("<h1>Analyze</h1>\n<p>{}</p>\n<p>{} bytes · {} files · {} folders · {} duplicate groups ({} bytes)</p>\n", html_e(&r.roots.iter().map(|x| p(x)).collect::<Vec<_>>().join("   ")), r.total_bytes, r.file_count, r.directory_count, r.duplicate_groups, r.reclaimable_bytes));
            s.push_str("<h2>Largest files</h2><table>\n");
            for f in &r.largest {
                s.push_str(&format!(
                    "<tr><td class=\"n\">{}</td><td>{}</td></tr>\n",
                    f.size,
                    html_e(&p(&f.path))
                ));
            }
            s.push_str("</table>\n<h2>By type</h2><table>\n");
            for t in &r.by_type {
                s.push_str(&format!("<tr><td>{}</td><td class=\"n\">{}</td><td class=\"n\">{:.1}%</td><td class=\"n\">{} files</td></tr>\n", html_e(&t.extension), t.bytes, t.bytes as f64 / total * 100.0, t.count));
            }
            s.push_str("</table>\n");
            if !r.empty_directories.is_empty() {
                s.push_str("<h2>Empty folders</h2><table>\n");
                for d in &r.empty_directories {
                    s.push_str(&format!("<tr><td>{}</td></tr>\n", html_e(&p(d))));
                }
                s.push_str("</table>\n");
            }
            s.push_str("</body></html>\n");
            s.into_bytes()
        }
    }
}

pub fn analyze(
    roots: &[PathBuf],
    f: &FolderSettings,
    progress: &dyn Progress,
) -> Result<AnalyzeReport, Cancelled> {
    let started = std::time::Instant::now();
    let excl = ExclusionFilter::from_rules(&merge_rules(&f.scan_exclusions, &f.analyze.exclusions));
    let scan = folder::scan(roots, &excl, &f.empty, progress)?;
    let dups = folder::find_duplicates(&scan.files, &f.duplicates, progress)?;
    Ok(build_report(roots, &scan, &dups, started.elapsed()))
}

fn merge_rules(
    a: &crate::settings::ExclusionRules,
    b: &crate::settings::ExclusionRules,
) -> crate::settings::ExclusionRules {
    let join = |x: &str, y: &str| {
        [x, y]
            .iter()
            .filter(|s| !s.trim().is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join("\n")
    };
    crate::settings::ExclusionRules {
        folders: join(&a.folders, &b.folders),
        files: join(&a.files, &b.files),
        extensions: join(&a.extensions, &b.extensions),
    }
}

// ------------------------------------------------------------------ delete plans

#[derive(Debug, PartialEq, Eq)]
pub struct DeletePlan {
    pub paths: Vec<PathBuf>,
    pub reclaim_bytes: u64,
}

pub fn plan_remove_duplicates(
    roots: &[PathBuf],
    f: &FolderSettings,
    progress: &dyn Progress,
    mode: folder::DuplicateMode,
) -> Result<(Vec<Vec<ScannedFile>>, DeletePlan), Cancelled> {
    let scan = folder::scan(
        roots,
        &ExclusionFilter::from_rules(&f.scan_exclusions),
        &f.empty,
        progress,
    )?;
    let groups = folder::find_duplicates(&scan.files, &f.duplicates, progress)?;
    let doomed = folder::to_delete(&groups, mode);
    let plan = DeletePlan {
        reclaim_bytes: doomed.iter().map(|d| d.size).sum(),
        paths: doomed.into_iter().map(|d| d.path).collect(),
    };
    Ok((groups, plan))
}

pub fn plan_remove_empty(
    roots: &[PathBuf],
    f: &FolderSettings,
    progress: &dyn Progress,
) -> Result<(DeletePlan, usize), Cancelled> {
    let scan = folder::scan(
        roots,
        &ExclusionFilter::from_rules(&f.scan_exclusions),
        &f.empty,
        progress,
    )?;
    let total = scan.empty_directories.len();
    Ok((
        DeletePlan {
            paths: folder::outermost(&scan.empty_directories),
            reclaim_bytes: 0,
        },
        total,
    ))
}

/// Permanent deletion, used only when the user chose it in settings. Refuses anything that
/// is not inside one of `roots` (defence against a corrupted plan) and never follows symlinks.
pub fn delete_permanently(paths: &[PathBuf], roots: &[PathBuf]) -> io::Result<usize> {
    let canon_roots: Vec<PathBuf> = roots
        .iter()
        .filter_map(|r| fs::canonicalize(r).ok())
        .collect();
    let mut n = 0;
    for path in paths {
        let parent = path.parent().and_then(|d| fs::canonicalize(d).ok());
        let inside = parent.is_some_and(|d| canon_roots.iter().any(|r| d.starts_with(r)));
        if !inside {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} is outside the selected folders", path.display()),
            ));
        }
        let meta = fs::symlink_metadata(path)?;
        if meta.is_dir() {
            fs::remove_dir(path).or_else(|_| fs::remove_dir_all(path))?;
        } else {
            fs::remove_file(path)?;
        }
        n += 1;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::folder::{DuplicateMode, NullProgress};
    use crate::settings::Settings;

    fn touch(p: &Path, data: &[u8]) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, data).unwrap();
    }

    fn plan(prefix: &str, name: &str, suffix: &str, numbered: bool) -> RenamePlan {
        RenamePlan {
            prefix: prefix.into(),
            name: name.into(),
            suffix: suffix.into(),
            numbered,
        }
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn rename_numbers_in_natural_order_per_extension() {
        let d = tempfile::tempdir().unwrap();
        for n in ["img10.png", "img2.png", "IMG1.PNG", "doc.txt", "a.TXT"] {
            touch(&d.path().join(n), n.as_bytes());
        }
        let s = RenameSettings::default();
        let pl = plan_rename(
            d.path(),
            &plan("holiday_", "", "", true),
            &s,
            &ExclusionFilter::default(),
        )
        .unwrap();
        let out = apply_rename(&pl, &|| false).unwrap();
        assert_eq!((out.renamed, out.errors.len()), (5, 0));
        assert_eq!(
            names(d.path()),
            [
                "holiday_1.PNG",
                "holiday_1.TXT",
                "holiday_2.png",
                "holiday_2.txt",
                "holiday_3.png"
            ]
        );
        assert_eq!(
            fs::read(d.path().join("holiday_1.PNG")).unwrap(),
            b"IMG1.PNG",
            "natural order: 1, 2, 10"
        );
        assert_eq!(
            fs::read(d.path().join("holiday_3.png")).unwrap(),
            b"img10.png"
        );
    }

    #[test]
    fn rename_padding_start_and_collisions() {
        let d = tempfile::tempdir().unwrap();
        for i in 0..12 {
            touch(&d.path().join(format!("f{i:02}.jpg")), b"x");
        }
        let s = RenameSettings {
            start_number: 8,
            ..Default::default()
        };
        let pl = plan_rename(
            d.path(),
            &plan("", "", "", true),
            &s,
            &ExclusionFilter::default(),
        )
        .unwrap();
        assert_eq!(pl[0].to.file_name().unwrap(), "08.jpg"); // 8..19 -> width 2
        let fixed = RenameSettings {
            digit_count: 4,
            ..Default::default()
        };
        let pl = plan_rename(
            d.path(),
            &plan("", "", "", true),
            &fixed,
            &ExclusionFilter::default(),
        )
        .unwrap();
        assert_eq!(pl[0].to.file_name().unwrap(), "0001.jpg");
        // fixed name: duplicates get (2), (3)
        let pl = plan_rename(
            d.path(),
            &plan("", "same", "", false),
            &RenameSettings::default(),
            &ExclusionFilter::default(),
        )
        .unwrap();
        let targets: HashSet<_> = pl
            .iter()
            .map(|r| r.to.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(targets.len(), 12);
        assert!(
            targets.contains("same.jpg")
                && targets.contains("same (2).jpg")
                && targets.contains("same (12).jpg")
        );
    }

    #[test]
    fn rename_swap_and_shift_do_not_clobber() {
        let d = tempfile::tempdir().unwrap();
        touch(&d.path().join("1.txt"), b"was-1");
        touch(&d.path().join("2.txt"), b"was-2");
        // names 1.txt and 2.txt already are the numbering targets, in swapped order
        let pl = vec![
            Rename {
                from: d.path().join("1.txt"),
                to: d.path().join("2.txt"),
            },
            Rename {
                from: d.path().join("2.txt"),
                to: d.path().join("1.txt"),
            },
        ];
        let out = apply_rename(&pl, &|| false).unwrap();
        assert_eq!(out.renamed, 2);
        assert_eq!(fs::read(d.path().join("1.txt")).unwrap(), b"was-2");
        assert_eq!(fs::read(d.path().join("2.txt")).unwrap(), b"was-1");
    }

    #[test]
    fn rename_never_overwrites_an_outside_file_and_rolls_back() {
        let d = tempfile::tempdir().unwrap();
        touch(&d.path().join("a.txt"), b"mine");
        touch(&d.path().join("keep.txt"), b"precious");
        let pl = vec![Rename {
            from: d.path().join("a.txt"),
            to: d.path().join("keep.txt"),
        }];
        let out = apply_rename(&pl, &|| false).unwrap();
        assert_eq!((out.renamed, out.errors.len()), (0, 1));
        assert_eq!(fs::read(d.path().join("keep.txt")).unwrap(), b"precious");
        assert_eq!(
            fs::read(d.path().join("a.txt")).unwrap(),
            b"mine",
            "restored after the failure"
        );
        assert_eq!(names(d.path()), ["a.txt", "keep.txt"]);
        // cancellation before anything moved
        assert!(apply_rename(&pl, &|| true).is_err());
        assert_eq!(names(d.path()), ["a.txt", "keep.txt"]);
    }

    #[test]
    fn rename_validation_and_dry_run() {
        assert_eq!(
            plan("", "", "", false).validate(),
            Err(RenameError::InvalidName)
        );
        assert_eq!(
            plan("a/", "", "", true).validate(),
            Err(RenameError::InvalidName)
        );
        assert_eq!(
            plan("..", "", "", false).validate(),
            Err(RenameError::InvalidName)
        );
        assert!(plan("日本_", "", "", true).validate().is_ok());
        let d = tempfile::tempdir().unwrap();
        touch(&d.path().join("x.png"), b"");
        let pl = plan_rename(
            d.path(),
            &plan("p", "", "", true),
            &RenameSettings::default(),
            &ExclusionFilter::default(),
        )
        .unwrap();
        assert_eq!(dry_run_lines(&pl), ["x.png  ->  p1.png"]);
        assert_eq!(names(d.path()), ["x.png"], "planning alone changes nothing");
    }

    #[test]
    fn rename_skips_excluded_and_subfolders_and_symlinks() {
        let d = tempfile::tempdir().unwrap();
        touch(&d.path().join("a.txt"), b"");
        touch(&d.path().join("skip.log"), b"");
        touch(&d.path().join("sub/b.txt"), b"");
        std::os::unix::fs::symlink(d.path().join("a.txt"), d.path().join("link.txt")).unwrap();
        let excl = ExclusionFilter::from_rules(&crate::settings::ExclusionRules {
            extensions: "log".into(),
            ..Default::default()
        });
        let pl = plan_rename(
            d.path(),
            &plan("n", "", "", true),
            &RenameSettings::default(),
            &excl,
        )
        .unwrap();
        assert_eq!(pl.len(), 1);
        assert_eq!(pl[0].from.file_name().unwrap(), "a.txt");
    }

    #[test]
    fn analyze_report_in_every_format() {
        let d = tempfile::tempdir().unwrap();
        touch(&d.path().join("a/big.bin"), &vec![1u8; 5000]);
        touch(&d.path().join("a/big2.bin"), &vec![1u8; 5000]);
        touch(&d.path().join("b/note,1.txt"), b"hello");
        touch(&d.path().join("noext"), b"x");
        fs::create_dir(d.path().join("empty")).unwrap();
        let f = Settings::default().folder;
        let r = analyze(&[d.path().to_path_buf()], &f, &NullProgress).unwrap();
        assert_eq!((r.file_count, r.total_bytes), (4, 10_006));
        assert_eq!(
            (r.duplicate_groups, r.duplicate_files, r.reclaimable_bytes),
            (1, 2, 5000)
        );
        assert_eq!(
            r.by_type[0],
            TypeShare {
                extension: "bin".into(),
                count: 2,
                bytes: 10_000
            }
        );
        assert!(r.by_type.iter().any(|t| t.extension == "(no extension)"));
        assert_eq!(r.empty_directories.len(), 1);

        let csv = String::from_utf8(render_report(&r, ReportFormat::Csv)).unwrap();
        assert!(
            csv.starts_with("\u{feff}section,name,bytes,count,share\n")
                && csv.contains("\"")
                && csv.contains("summary,total,10006,4,")
        );
        assert!(csv.contains("\""), "names with commas are quoted");
        let json: serde_json::Value =
            serde_json::from_slice(&render_report(&r, ReportFormat::Json)).unwrap();
        assert_eq!(
            (
                json["files"].as_u64(),
                json["duplicates"]["groups"].as_u64()
            ),
            (Some(4), Some(1))
        );
        let html = String::from_utf8(render_report(&r, ReportFormat::Html)).unwrap();
        assert!(html.contains("<table>") && html.contains("10000"));
        let txt = String::from_utf8(render_report(&r, ReportFormat::Txt)).unwrap();
        assert!(txt.contains("2 files") || txt.contains("bin"));
        assert_eq!(report_extension(ReportFormat::Html), ".html");
    }

    #[test]
    fn html_report_escapes_hostile_file_names() {
        let d = tempfile::tempdir().unwrap();
        touch(&d.path().join("<script>alert(1)</script>.txt"), b"x");
        let r = analyze(
            &[d.path().to_path_buf()],
            &Settings::default().folder,
            &NullProgress,
        )
        .unwrap();
        let html = String::from_utf8(render_report(&r, ReportFormat::Html)).unwrap();
        assert!(!html.contains("<script>alert") && html.contains("&lt;script&gt;"));
    }

    #[test]
    fn delete_plans_and_the_permanent_guard() {
        let d = tempfile::tempdir().unwrap();
        touch(&d.path().join("a/one.bin"), b"same-content");
        touch(&d.path().join("b/two.bin"), b"same-content");
        fs::create_dir_all(d.path().join("x/y/z")).unwrap();
        let f = Settings::default().folder;
        let (groups, dp) = plan_remove_duplicates(
            &[d.path().to_path_buf()],
            &f,
            &NullProgress,
            DuplicateMode::KeepOldest,
        )
        .unwrap();
        assert_eq!((groups.len(), dp.paths.len(), dp.reclaim_bytes), (1, 1, 12));
        let (ep, total) = plan_remove_empty(&[d.path().to_path_buf()], &f, &NullProgress).unwrap();
        assert_eq!((ep.paths.clone(), total), (vec![d.path().join("x")], 3));

        // nothing is removed by planning
        assert!(d.path().join("a/one.bin").exists() && d.path().join("x/y/z").exists());
        assert_eq!(
            delete_permanently(&ep.paths, &[d.path().to_path_buf()]).unwrap(),
            1
        );
        assert!(!d.path().join("x").exists());
        // a plan pointing outside the selected folders is refused
        let other = tempfile::tempdir().unwrap();
        touch(&other.path().join("victim"), b"v");
        let err = delete_permanently(&[other.path().join("victim")], &[d.path().to_path_buf()])
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(other.path().join("victim").exists());
        // symlinks are removed as links, never followed
        std::os::unix::fs::symlink(other.path(), d.path().join("lnk")).unwrap();
        delete_permanently(&[d.path().join("lnk")], &[d.path().to_path_buf()]).unwrap();
        assert!(other.path().join("victim").exists() && !d.path().join("lnk").exists());
    }
}
