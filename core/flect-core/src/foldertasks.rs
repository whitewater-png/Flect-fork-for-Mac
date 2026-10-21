//! Folder operations that need a plan before touching anything: rename, analyze,
//! and the delete plans for duplicates / empty folders. Deleting itself is either done
//! here (permanent mode) or requested from the app as `needs{trash}` so it lands in the Trash.
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::folder::{self, Cancelled, ExclusionFilter, Progress, ScannedFile, TreeScan};
use crate::fsat;
use crate::settings::{EmptyFolderSettings, FolderSettings, RenameSettings, ReportFormat};
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

#[derive(Debug, PartialEq, Eq, Default)]
pub struct DeletePlan {
    pub paths: Vec<PathBuf>,
    pub reclaim_bytes: u64,
    /// Remove Empty: (device, inode) of each planned folder and of each selected folder, as
    /// scanned. A folder is only removed while both still match.
    pub folder_ids: HashMap<PathBuf, (u64, u64)>,
    pub root_ids: Vec<(PathBuf, (u64, u64))>,
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
        ..Default::default()
    };
    Ok((groups, plan))
}

pub fn plan_remove_empty(
    roots: &[PathBuf],
    f: &FolderSettings,
    progress: &dyn Progress,
) -> Result<(DeletePlan, usize), Cancelled> {
    plan_remove_empty_protecting(roots, f, progress, &HashSet::new())
}

/// [`plan_remove_empty`], leaving out every empty folder whose (device, inode) is in `protected`
/// (the selected items and the folders that hold them: removing such a folder would remove a
/// selected item).
pub fn plan_remove_empty_protecting(
    roots: &[PathBuf],
    f: &FolderSettings,
    progress: &dyn Progress,
    protected: &HashSet<(u64, u64)>,
) -> Result<(DeletePlan, usize), Cancelled> {
    let mut scan = folder::scan(
        roots,
        &ExclusionFilter::from_rules(&f.scan_exclusions),
        &f.empty,
        progress,
    )?;
    let ids = std::mem::take(&mut scan.dir_ids);
    scan.empty_directories
        .retain(|d| ids.get(d).is_some_and(|id| !protected.contains(id)));
    let total = scan.empty_directories.len();
    let paths = folder::outermost(&scan.empty_directories);
    let folder_ids = paths
        .iter()
        .filter_map(|p| ids.get(p).map(|id| (p.clone(), *id)))
        .collect();
    Ok((
        DeletePlan {
            paths,
            reclaim_bytes: 0,
            folder_ids,
            root_ids: scan.root_ids,
        },
        total,
    ))
}

fn canonical_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    roots
        .iter()
        .filter_map(|r| fs::canonicalize(r).ok())
        .collect()
}

/// Refuses anything that is not inside one of the (canonical) roots: defence against a corrupted plan.
fn check_inside(path: &Path, canon_roots: &[PathBuf]) -> io::Result<()> {
    let parent = path.parent().and_then(|d| fs::canonicalize(d).ok());
    if parent.is_some_and(|d| canon_roots.iter().any(|r| d.starts_with(r))) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is outside the selected folders", path.display()),
        ))
    }
}

/// Re-check, right before acting on it, that `dir` still holds nothing but empty folders and OS
/// litter (see `folder::ignorable`). Read-only. Returns the number of folders in it (itself
/// included), or `None` when anything counts as content: a file, a link, a special file, an
/// excluded folder, or anything that cannot be read.
pub fn still_empty(
    dir: &Path,
    excl: &ExclusionFilter,
    empty: &EmptyFolderSettings,
) -> Option<usize> {
    if !fs::symlink_metadata(dir).ok()?.is_dir() {
        return None;
    }
    let mut folders = 1;
    for entry in fs::read_dir(dir).ok()? {
        let entry = entry.ok()?;
        let meta = fs::symlink_metadata(entry.path()).ok()?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if meta.is_dir() {
            if excl.excludes_folder(&name) {
                return None;
            }
            folders += still_empty(&entry.path(), excl, empty)?;
        } else if !(meta.is_file() && folder::ignorable(&name, meta.len(), empty)) {
            return None;
        }
    }
    Some(folders)
}

/// Delete the empty tree `name` in the open folder `parent`, deepest first, through handles only:
/// each folder is opened with `O_NOFOLLOW` relative to its (open) parent; each litter file and
/// then each folder is removed with [`fsat::remove_checked`] (renamed to a fresh temporary name,
/// checked again under it, then `unlinkat` / `unlinkat(AT_REMOVEDIR)`, which refuses a folder that
/// is not empty). A litter file must still be the same file, still litter (a 0-byte file still 0
/// bytes); a folder still the same folder. Stops at the first thing that is not litter, so content
/// that appeared after the check is left alone; renaming or swapping a folder on the way, or
/// renaming another item into a checked name, cannot redirect a removal elsewhere.
fn remove_empty_tree_at(
    parent: &std::os::fd::OwnedFd,
    name: &std::ffi::OsStr,
    expected: (u64, u64),
    excl: &ExclusionFilter,
    empty: &EmptyFolderSettings,
) -> io::Result<usize> {
    let not_empty = || io::Error::other(format!("{} is not empty", name.to_string_lossy()));
    let dir = fsat::open_dir_at(parent, name)?;
    let dir_id = fsat::Fingerprint::of_stat(&fsat::stat_fd(&dir)?).id();
    if dir_id != expected {
        return Err(fsat::changed(&format!(
            "{} is not the folder that was checked",
            name.to_string_lossy()
        )));
    }
    let mut removed = 0;
    for entry in fsat::names_in(&dir)? {
        let st = fsat::stat_at(&dir, &entry)?;
        let ename = entry.to_string_lossy().into_owned();
        if fsat::is_dir(&st) && !excl.excludes_folder(&ename) {
            let sub = fsat::Fingerprint::of_stat(&st).id();
            removed += remove_empty_tree_at(&dir, &entry, sub, excl, empty)?;
        } else if fsat::is_file(&st) && folder::ignorable(&ename, st.st_size as u64, empty) {
            fsat::seam("E3", Path::new(&entry));
            let was = fsat::Fingerprint::of_stat(&st);
            let still_litter = |now: &rustix::fs::Stat| {
                let n = fsat::Fingerprint::of_stat(now);
                (n.dev, n.ino, n.size, n.mtime) == (was.dev, was.ino, was.size, was.mtime)
                    && folder::ignorable(&ename, n.size, empty)
            };
            match fsat::remove_checked(&dir, &entry, false, &still_litter, &|| true) {
                Ok(()) | Err(fsat::NotRemoved::Gone) => {}
                Err(why) => return Err(not_removed(name, why)),
            }
        } else {
            return Err(not_empty());
        }
    }
    drop(dir);
    fsat::seam("E2", Path::new(name));
    let same_folder = |now: &rustix::fs::Stat| fsat::Fingerprint::of_stat(now).id() == dir_id;
    match fsat::remove_checked(parent, name, true, &same_folder, &|| true) {
        Ok(()) => Ok(removed + 1),
        Err(why) => Err(not_removed(name, why)),
    }
}

/// An error for a removal step that did not happen.
fn not_removed(name: &std::ffi::OsStr, why: fsat::NotRemoved) -> io::Error {
    match why {
        fsat::NotRemoved::Failed(e) => e,
        fsat::NotRemoved::LeftAs(t) => io::Error::other(format!(
            "{} changed; left as {}",
            name.to_string_lossy(),
            t.to_string_lossy()
        )),
        fsat::NotRemoved::Gone | fsat::NotRemoved::Changed => {
            io::Error::other(format!("{} changed", name.to_string_lossy()))
        }
    }
}

/// [`remove_empty_tree_at`] for `path`, reached through handles from the selected folder it lies in.
/// The selected folder must still be the one scanned (its (device, inode) as planned), and so
/// must the planned folder, or nothing is removed.
fn remove_empty_tree(
    path: &Path,
    plan: &DeletePlan,
    roots: &[PathBuf],
    excl: &ExclusionFilter,
    empty: &EmptyFolderSettings,
) -> io::Result<usize> {
    let root = roots
        .iter()
        .filter(|r| path.starts_with(r) && path != r.as_path())
        .max_by_key(|r| r.components().count())
        .ok_or_else(|| fsat::changed("not inside a selected folder"))?;
    let rel = path
        .strip_prefix(root)
        .map_err(|_| fsat::changed("not below the root"))?;
    let name = rel.file_name().ok_or_else(|| fsat::changed("no name"))?;
    let root_id = plan
        .root_ids
        .iter()
        .find(|(r, _)| r == root)
        .map(|(_, id)| *id)
        .ok_or_else(|| fsat::changed("the selected folder was not scanned"))?;
    let id = *plan
        .folder_ids
        .get(path)
        .ok_or_else(|| fsat::changed("not a planned folder"))?;
    fsat::seam("E1", path);
    let parent = fsat::open_below(root, Some(root_id), rel.parent().unwrap_or(Path::new("")))?;
    remove_empty_tree_at(&parent, name, id, excl, empty)
}

#[derive(Debug, Default)]
pub struct EmptyRemoval {
    /// Folders removed (nested ones included).
    pub removed: usize,
    /// Planned folders left alone: `None` when no longer empty, otherwise why the removal failed.
    pub skipped: Vec<(PathBuf, Option<String>)>,
}

/// Permanent removal of planned empty folders. Each one is re-checked right before it is touched
/// and only removed if it is still empty (OS litter aside); failures are reported, never retried
/// with a recursive delete.
pub fn delete_empty_permanently(
    plan: &DeletePlan,
    roots: &[PathBuf],
    f: &FolderSettings,
) -> io::Result<EmptyRemoval> {
    let canon_roots = canonical_roots(roots);
    let excl = ExclusionFilter::from_rules(&f.scan_exclusions);
    let mut out = EmptyRemoval::default();
    for path in &plan.paths {
        if let Err(e) = check_inside(path, &canon_roots) {
            if !roots
                .iter()
                .any(|r| path.starts_with(r) && path != r.as_path())
            {
                return Err(e); // not a plan for these folders at all
            }
            // Under a selected folder as planned, but a folder on the way changed (moved, or a
            // link put in its place): left alone and reported.
            out.skipped.push((path.clone(), Some(e.to_string())));
            continue;
        }
        if still_empty(path, &excl, &f.empty).is_none() {
            out.skipped.push((path.clone(), None));
            continue;
        }
        match remove_empty_tree(path, plan, roots, &excl, &f.empty) {
            Ok(n) => out.removed += n,
            Err(e) => out.skipped.push((path.clone(), Some(e.to_string()))),
        }
    }
    Ok(out)
}

/// For Trash mode: the planned folders that are still empty right now (the app trashes exactly
/// these), the number of folders in them, and the ones that are not empty any more.
pub fn recheck_empty(paths: &[PathBuf], f: &FolderSettings) -> (Vec<PathBuf>, usize, Vec<PathBuf>) {
    let excl = ExclusionFilter::from_rules(&f.scan_exclusions);
    let (mut keep, mut total, mut skipped) = (Vec::new(), 0, Vec::new());
    for p in paths {
        match still_empty(p, &excl, &f.empty) {
            Some(n) => {
                keep.push(p.clone());
                total += n;
            }
            None => skipped.push(p.clone()),
        }
    }
    (keep, total, skipped)
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

    /// Content that appears between the plan and the deletion is never deleted, in either mode.
    #[test]
    fn empty_folders_are_rechecked_right_before_deletion() {
        let d = tempfile::tempdir().unwrap();
        let root = [d.path().to_path_buf()];
        fs::create_dir_all(d.path().join("e/inner")).unwrap();
        fs::create_dir_all(d.path().join("f")).unwrap();
        touch(&d.path().join("f/.DS_Store"), b"litter");
        touch(&d.path().join("f/Icon\r"), b"");
        fs::create_dir_all(d.path().join("g")).unwrap();
        let f = Settings::default().folder;
        let (plan, total) = plan_remove_empty(&root, &f, &NullProgress).unwrap();
        assert_eq!(total, 4);
        assert_eq!(
            plan.paths,
            ["e", "f", "g"].map(|p| d.path().join(p)).to_vec()
        );
        touch(&d.path().join("e/inner/new.txt"), b"arrived late");
        std::os::unix::fs::symlink("/", d.path().join("g/link")).unwrap();
        let (keep, n, skipped) = recheck_empty(&plan.paths, &f);
        assert_eq!((keep, n), (vec![d.path().join("f")], 1));
        assert_eq!(skipped, ["e", "g"].map(|p| d.path().join(p)).to_vec());
        let out = delete_empty_permanently(&plan, &root, &f).unwrap();
        assert_eq!(out.removed, 1);
        assert_eq!(out.skipped.len(), 2);
        assert!(d.path().join("e/inner/new.txt").exists() && d.path().join("g/link").exists());
        assert!(!d.path().join("f").exists(), "litter-only folder removed");
        // the litter rule is a per-file rule: litter next to content does not make a folder empty
        fs::create_dir_all(d.path().join("h")).unwrap();
        touch(&d.path().join("h/.DS_Store"), b"x");
        touch(&d.path().join("h/real.txt"), b"x");
        assert!(still_empty(&d.path().join("h"), &ExclusionFilter::default(), &f.empty).is_none());
    }

    /// rev021e E2 / E3 and the opt-in 0-byte rule: whatever is renamed into, written to or added to
    /// a checked name or folder right before its removal is never removed, and the folder stays.
    #[test]
    fn litter_and_folders_changed_right_before_removal_are_kept() {
        for (phase, attack, zero) in [
            ("E3", "litter_swap", false),
            ("E3", "zero_write", true),
            ("E2", "add_file", false),
            ("E2", "parent_full", false),
            ("R1", "litter_swap", false),
        ] {
            let d = tempfile::tempdir().unwrap();
            let r = d.path().to_path_buf();
            fs::create_dir_all(r.join("e/sub")).unwrap();
            fs::create_dir_all(r.join("full")).unwrap();
            fs::write(r.join("e/.DS_Store"), "litter").unwrap();
            fs::write(r.join("full/real.txt"), "REAL").unwrap();
            if zero {
                fs::write(r.join("e/sub/zero.txt"), "").unwrap();
            }
            let mut f = Settings::default().folder;
            f.empty.ignore_zero_byte_files = zero;
            let (plan, _) = plan_remove_empty(std::slice::from_ref(&r), &f, &NullProgress).unwrap();
            assert_eq!(plan.paths, [r.join("e")], "{phase} {attack}");
            let (rr, once) = (r.clone(), std::rc::Rc::new(std::cell::Cell::new(false)));
            let flag = once.clone();
            let out = crate::fsat::with_seam(
                move |p, name| {
                    let target = match attack {
                        "litter_swap" => ".DS_Store",
                        "zero_write" => "zero.txt",
                        _ => "e",
                    };
                    if p != phase || name != Path::new(target) || flag.replace(true) {
                        return;
                    }
                    match attack {
                        "litter_swap" => {
                            fs::rename(rr.join("e/.DS_Store"), rr.join("e/.DS_Store.old")).unwrap();
                            fs::write(rr.join("e/.DS_Store"), "IMPORTANT, REALLY").unwrap();
                        }
                        "zero_write" => {
                            fs::write(rr.join("e/sub/zero.txt"), "IMPORTANT-NEW-DATA").unwrap()
                        }
                        "add_file" => fs::write(rr.join("e/new.txt"), "NEW").unwrap(),
                        _ => {
                            fs::rename(rr.join("e"), rr.join("e.old")).unwrap();
                            fs::rename(rr.join("full"), rr.join("e")).unwrap();
                        }
                    }
                },
                || delete_empty_permanently(&plan, std::slice::from_ref(&r), &f).unwrap(),
            );
            assert!(once.get(), "{phase} {attack}: the seam ran");
            assert_eq!(out.removed, 0, "{phase} {attack}: {out:?}");
            assert_eq!(out.skipped.len(), 1, "{phase} {attack}: {out:?}");
            match attack {
                "litter_swap" => assert_eq!(
                    fs::read_to_string(r.join("e/.DS_Store")).unwrap(),
                    "IMPORTANT, REALLY"
                ),
                "zero_write" => assert_eq!(
                    fs::read_to_string(r.join("e/sub/zero.txt")).unwrap(),
                    "IMPORTANT-NEW-DATA"
                ),
                "add_file" => assert_eq!(fs::read_to_string(r.join("e/new.txt")).unwrap(), "NEW"),
                _ => assert_eq!(fs::read_to_string(r.join("e/real.txt")).unwrap(), "REAL"),
            }
            let temp: Vec<_> = walk_names(&r)
                .into_iter()
                .filter(|n| n.contains(".flect-rm-"))
                .collect();
            assert!(temp.is_empty(), "{phase} {attack}: {temp:?}");
        }
    }

    /// rev021e E1: the selected folder (or the planned folder) swapped for another one between the
    /// plan and the permanent removal. Both are checked by the (device, inode) recorded at scan:
    /// nothing is removed.
    #[test]
    fn swapped_selected_or_planned_folders_are_never_removed() {
        for attack in ["root_swap", "root_link", "planned_swap"] {
            let d = tempfile::tempdir().unwrap();
            let (sel, other) = (d.path().join("sel"), d.path().join("other"));
            fs::create_dir_all(sel.join("e/sub")).unwrap();
            fs::write(sel.join("e/.DS_Store"), "litter").unwrap();
            fs::create_dir_all(other.join("e/sub")).unwrap();
            fs::write(other.join("e/.DS_Store"), "OTHERLITTER").unwrap();
            let f = Settings::default().folder;
            let (plan, _) =
                plan_remove_empty(std::slice::from_ref(&sel), &f, &NullProgress).unwrap();
            assert_eq!(plan.paths, [sel.join("e")]);
            assert!(plan.folder_ids.contains_key(&sel.join("e")) && plan.root_ids.len() == 1);
            let (s2, o2, once) = (
                sel.clone(),
                other.clone(),
                std::rc::Rc::new(std::cell::Cell::new(false)),
            );
            let flag = once.clone();
            let out = crate::fsat::with_seam(
                move |p, _| {
                    if p != "E1" || flag.replace(true) {
                        return;
                    }
                    match attack {
                        "root_swap" => {
                            fs::rename(&s2, s2.with_extension("old")).unwrap();
                            fs::rename(&o2, &s2).unwrap();
                        }
                        "root_link" => {
                            fs::rename(&s2, s2.with_extension("old")).unwrap();
                            std::os::unix::fs::symlink(&o2, &s2).unwrap();
                        }
                        _ => {
                            fs::rename(s2.join("e"), s2.join("e.old")).unwrap();
                            fs::rename(o2.join("e"), s2.join("e")).unwrap();
                        }
                    }
                },
                || delete_empty_permanently(&plan, std::slice::from_ref(&sel), &f).unwrap(),
            );
            assert!(once.get(), "{attack}");
            assert_eq!(
                (out.removed, out.skipped.len()),
                (0, 1),
                "{attack}: {out:?}"
            );
            let litter_of = |p: &Path| fs::read_to_string(p.join("e/.DS_Store")).unwrap();
            match attack {
                "root_swap" => {
                    assert_eq!(litter_of(&sel), "OTHERLITTER");
                    assert_eq!(litter_of(&sel.with_extension("old")), "litter");
                }
                "root_link" => {
                    assert_eq!(litter_of(&other), "OTHERLITTER");
                    assert_eq!(litter_of(&sel.with_extension("old")), "litter");
                }
                _ => {
                    assert_eq!(litter_of(&sel), "OTHERLITTER");
                    assert_eq!(
                        fs::read_to_string(sel.join("e.old/.DS_Store")).unwrap(),
                        "litter"
                    );
                }
            }
        }
    }

    fn walk_names(r: &Path) -> Vec<String> {
        let mut out = Vec::new();
        for e in fs::read_dir(r).unwrap() {
            let e = e.unwrap();
            out.push(e.path().to_string_lossy().into_owned());
            if e.file_type().unwrap().is_dir() {
                out.extend(walk_names(&e.path()));
            }
        }
        out
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
        let gone = delete_empty_permanently(&ep, &[d.path().to_path_buf()], &f).unwrap();
        assert_eq!((gone.removed, gone.skipped.len()), (3, 0));
        assert!(!d.path().join("x").exists());
        // a folder on the way swapped for a link after planning: the removal is refused, never
        // redirected to the link's target
        let (p2, _) = {
            fs::create_dir_all(d.path().join("s/t/u")).unwrap();
            plan_remove_empty(&[d.path().to_path_buf()], &f, &NullProgress).unwrap()
        };
        assert_eq!(p2.paths, [d.path().join("s")]);
        let away = tempfile::tempdir().unwrap();
        fs::create_dir_all(away.path().join("t/u")).unwrap();
        let planned = [d.path().join("s/t")];
        fs::rename(d.path().join("s"), d.path().join("s.old")).unwrap();
        std::os::unix::fs::symlink(away.path(), d.path().join("s")).unwrap();
        let stale = DeletePlan {
            paths: planned.to_vec(),
            ..p2
        };
        let out = delete_empty_permanently(&stale, &[d.path().to_path_buf()], &f).unwrap();
        assert_eq!((out.removed, out.skipped.len()), (0, 1), "{out:?}");
        assert!(away.path().join("t/u").is_dir(), "followed the link");
        assert!(d.path().join("s.old/t/u").is_dir());
        // a plan pointing outside the selected folders is refused
        let other = tempfile::tempdir().unwrap();
        fs::create_dir_all(other.path().join("victim")).unwrap();
        let err = delete_empty_permanently(
            &DeletePlan {
                paths: vec![other.path().join("victim")],
                ..Default::default()
            },
            &[d.path().to_path_buf()],
            &f,
        )
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(other.path().join("victim").exists());
    }
}
