//! Folder tasks: exclusion rules, scanning, duplicate detection, empty folders, tree.
//! Symlinks are never followed (no loops, no escaping the selected tree).
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rayon::prelude::*;
use xxhash_rust::xxh3::Xxh3;

use crate::settings::{
    DuplicateSettings, DuplicateStrictness, EmptyFolderSettings, ExclusionRules, TreeSettings,
};

const HEAD_HASH_BYTES: u64 = 64 * 1024;
const TREE_MAX_ENTRIES: usize = 200_000;

/// Cooperative cancellation and progress reporting.
pub trait Progress: Sync {
    fn cancelled(&self) -> bool {
        false
    }
    fn status(&self, _key: &str, _args: &[String]) {}
    fn step(&self, _done: u64, _total: u64) {}
    /// Progress within the current file, 0..=1000.
    fn file_step(&self, _permille: u32) {}
}

pub struct NullProgress;
impl Progress for NullProgress {}

#[derive(Debug, PartialEq, Eq)]
pub struct Cancelled;

// ------------------------------------------------------------ exclusions

pub fn split_patterns(text: &str) -> Vec<String> {
    text.split(['\n', '\r', ';'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

fn wildcard(pattern: &[char], value: &[char]) -> bool {
    let (mut p, mut v, mut star, mut mark) = (0, 0, None, 0);
    while v < value.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == value[v]) {
            p += 1;
            v += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            p += 1;
            mark = v;
        } else if let Some(s) = star {
            p = s + 1;
            mark += 1;
            v = mark;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

fn matches(pattern: &str, value: &str) -> bool {
    let (p, v): (Vec<char>, Vec<char>) = (
        pattern.to_uppercase().chars().collect(),
        value.to_uppercase().chars().collect(),
    );
    if !pattern.contains(['*', '?']) {
        return p == v;
    }
    wildcard(&p, &v)
}

#[derive(Debug, Default, Clone)]
pub struct ExclusionFilter {
    folders: Vec<String>,
    files: Vec<String>,
    extensions: Vec<String>,
}

impl ExclusionFilter {
    pub fn from_rules(rules: &ExclusionRules) -> Self {
        Self {
            folders: split_patterns(&rules.folders),
            files: split_patterns(&rules.files),
            extensions: split_patterns(&rules.extensions)
                .into_iter()
                .map(|e| {
                    if e.starts_with('.') {
                        e
                    } else {
                        format!(".{e}")
                    }
                })
                .collect(),
        }
    }

    pub fn excludes_folder(&self, name: &str) -> bool {
        self.folders.iter().any(|p| matches(p, name))
    }

    pub fn excludes_file(&self, name: &str) -> bool {
        if self.files.iter().any(|p| matches(p, name)) {
            return true;
        }
        match name.rfind('.') {
            Some(i) if i + 1 < name.len() => self.extensions.iter().any(|p| matches(p, &name[i..])),
            _ => false,
        }
    }
}

// ------------------------------------------------------------ scanning

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedFile {
    pub path: PathBuf,
    pub size: u64,
    pub modified: SystemTime,
    pub extension: String,
}

#[derive(Debug, Default)]
pub struct TreeScan {
    pub files: Vec<ScannedFile>,
    pub empty_directories: Vec<PathBuf>,
    pub directory_count: usize,
}

impl TreeScan {
    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

/// Files that do not make a folder "non-empty" (OS litter), per the user's toggles.
/// The Windows toggle names are kept for settings compatibility; the "desktop.ini"
/// toggle also covers macOS litter (`.DS_Store`, `.localized`, AppleDouble `._*`).
fn ignorable(name: &str, size: u64, empty: &EmptyFolderSettings) -> bool {
    if empty.ignore_zero_byte_files && size == 0 {
        return true;
    }
    let lower = name.to_lowercase();
    if empty.ignore_desktop_ini
        && (lower == "desktop.ini"
            || lower == ".ds_store"
            || lower == ".localized"
            || name.starts_with("._"))
    {
        return true;
    }
    empty.ignore_thumbs_db && lower == "thumbs.db"
}

pub fn extension_of(name: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() => name[i + 1..].to_lowercase(),
        _ => String::new(),
    }
}

pub fn scan(
    roots: &[PathBuf],
    excl: &ExclusionFilter,
    empty: &EmptyFolderSettings,
    progress: &dyn Progress,
) -> Result<TreeScan, Cancelled> {
    let mut out = TreeScan::default();
    for root in roots {
        if progress.cancelled() {
            return Err(Cancelled);
        }
        if root.symlink_metadata().map(|m| m.is_dir()).unwrap_or(false) {
            walk(root, &mut out, excl, empty, progress, true)?;
        }
    }
    Ok(out)
}

/// Returns whether the directory contains something that counts as content.
fn walk(
    dir: &Path,
    out: &mut TreeScan,
    excl: &ExclusionFilter,
    empty: &EmptyFolderSettings,
    progress: &dyn Progress,
    is_root: bool,
) -> Result<bool, Cancelled> {
    if progress.cancelled() {
        return Err(Cancelled);
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(true);
    }; // unreadable: assume it has content
    let mut has_content = false;
    let mut subdirs = Vec::new();
    for entry in entries.flatten() {
        let Ok(meta) = entry.path().symlink_metadata() else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        let ft = meta.file_type();
        if ft.is_symlink() {
            has_content = true; // a link is content, but is never followed or listed
            continue;
        }
        if ft.is_dir() {
            if !excl.excludes_folder(&name) {
                subdirs.push(entry.path());
            }
        } else if ft.is_file() {
            if !ignorable(&name, meta.len(), empty) {
                has_content = true;
            }
            if excl.excludes_file(&name) {
                continue;
            }
            out.files.push(ScannedFile {
                path: entry.path(),
                size: meta.len(),
                modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                extension: extension_of(&name),
            });
            if out.files.len().is_multiple_of(4096) {
                progress.status("msg.scanningCount", &[out.files.len().to_string()]);
            }
        } else {
            has_content = true; // sockets, devices...
        }
    }
    subdirs.sort();
    for sub in subdirs {
        out.directory_count += 1;
        if walk(&sub, out, excl, empty, progress, false)? {
            has_content = true;
        }
    }
    if !has_content && !is_root {
        out.empty_directories.push(dir.to_path_buf());
    }
    Ok(has_content)
}

/// Keep only the outermost directories (deleting a parent covers its children).
pub fn outermost(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let all: HashSet<&PathBuf> = dirs.iter().collect();
    let mut keep: Vec<PathBuf> = dirs
        .iter()
        .filter(|d| d.parent().is_none_or(|p| !all.contains(&p.to_path_buf())))
        .cloned()
        .collect();
    keep.sort_by_key(|p| p.to_string_lossy().to_lowercase());
    keep
}

// ------------------------------------------------------------ duplicates

fn identity(f: &ScannedFile, s: &DuplicateSettings) -> String {
    let mut key = f.size.to_string();
    if s.match_name {
        key.push('|');
        key.push_str(
            &f.path
                .file_name()
                .map(|n| n.to_string_lossy().to_lowercase())
                .unwrap_or_default(),
        );
    }
    if s.match_timestamp {
        let secs = f
            .modified
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        key.push_str(&format!("|{secs}"));
    }
    key
}

fn hash_file(path: &Path, limit: u64) -> io::Result<u128> {
    let mut file = File::open(path)?.take(limit);
    let mut hasher = Xxh3::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            return Ok(hasher.digest128());
        }
        hasher.update(&buf[..n]);
    }
}

/// Byte-for-byte equality. A 128-bit non-cryptographic hash must never be the
/// only thing standing between a user and a deleted file.
pub fn same_content(a: &Path, b: &Path) -> io::Result<bool> {
    let (mut fa, mut fb) = (File::open(a)?, File::open(b)?);
    if fa.metadata()?.len() != fb.metadata()?.len() {
        return Ok(false);
    }
    let (mut ba, mut bb) = (vec![0u8; 64 * 1024], vec![0u8; 64 * 1024]);
    loop {
        let na = read_full(&mut fa, &mut ba)?;
        let nb = read_full(&mut fb, &mut bb)?;
        if na != nb || ba[..na] != bb[..nb] {
            return Ok(false);
        }
        if na == 0 {
            return Ok(true);
        }
    }
}

fn read_full(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = r.read(&mut buf[filled..])?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    Ok(filled)
}

fn hash_groups(
    files: Vec<&ScannedFile>,
    limit: u64,
    s: &DuplicateSettings,
    progress: &dyn Progress,
) -> Result<Vec<Vec<ScannedFile>>, Cancelled> {
    let total = files.len() as u64;
    let done = std::sync::atomic::AtomicU64::new(0);
    let hashed: Vec<Option<u128>> = files
        .par_iter()
        .map(|f| {
            if progress.cancelled() {
                return None;
            }
            let h = hash_file(&f.path, limit).ok();
            let d = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            if d.is_multiple_of(128) {
                progress.step(d, total);
            }
            h
        })
        .collect();
    if progress.cancelled() {
        return Err(Cancelled);
    }
    let mut groups: HashMap<(String, u128), Vec<ScannedFile>> = HashMap::new();
    for (f, h) in files.into_iter().zip(hashed) {
        if let Some(h) = h {
            groups
                .entry((identity(f, s), h))
                .or_default()
                .push(f.clone());
        }
    }
    Ok(groups.into_values().filter(|g| g.len() > 1).collect())
}

/// Split a candidate group into sets of truly identical files.
fn confirm(
    group: Vec<ScannedFile>,
    progress: &dyn Progress,
) -> Result<Vec<Vec<ScannedFile>>, Cancelled> {
    let mut sets: Vec<Vec<ScannedFile>> = Vec::new();
    for f in group {
        if progress.cancelled() {
            return Err(Cancelled);
        }
        let mut placed = false;
        for set in &mut sets {
            if same_content(&set[0].path, &f.path).unwrap_or(false) {
                set.push(f.clone());
                placed = true;
                break;
            }
        }
        if !placed {
            sets.push(vec![f]);
        }
    }
    Ok(sets.into_iter().filter(|s| s.len() > 1).collect())
}

pub fn find_duplicates(
    files: &[ScannedFile],
    s: &DuplicateSettings,
    progress: &dyn Progress,
) -> Result<Vec<Vec<ScannedFile>>, Cancelled> {
    let eligible: Vec<&ScannedFile> = files
        .iter()
        .filter(|f| !(s.ignore_empty_files && f.size == 0))
        .filter(|f| f.size as i64 >= s.minimum_bytes)
        .collect();
    let mut by_identity: HashMap<String, Vec<&ScannedFile>> = HashMap::new();
    for f in eligible {
        by_identity.entry(identity(f, s)).or_default().push(f);
    }
    let candidates: Vec<&ScannedFile> = by_identity
        .into_values()
        .filter(|g| g.len() > 1)
        .flatten()
        .collect();
    if candidates.is_empty() {
        return Ok(vec![]);
    }
    if s.strictness == DuplicateStrictness::SizeOnly {
        let mut groups: HashMap<String, Vec<ScannedFile>> = HashMap::new();
        for f in candidates {
            groups.entry(identity(f, s)).or_default().push(f.clone());
        }
        return Ok(sorted(groups.into_values().collect()));
    }
    progress.status("msg.comparing", &[candidates.len().to_string()]);
    let by_head = hash_groups(candidates, HEAD_HASH_BYTES, s, progress)?;
    if s.strictness == DuplicateStrictness::HeadHash {
        return Ok(sorted(by_head));
    }
    // Small files are fully covered by the head hash; larger ones are re-hashed in full.
    let (small, large): (Vec<_>, Vec<_>) = by_head
        .into_iter()
        .partition(|g| g[0].size <= HEAD_HASH_BYTES);
    let mut confirmed = small;
    let unverified: Vec<&ScannedFile> = large.iter().flatten().collect();
    if !unverified.is_empty() {
        progress.status("msg.verifying", &[unverified.len().to_string()]);
        confirmed.extend(hash_groups(unverified, u64::MAX, s, progress)?);
    }
    let mut final_groups = Vec::new();
    for g in confirmed {
        final_groups.extend(confirm(g, progress)?);
    }
    Ok(sorted(final_groups))
}

fn sorted(mut groups: Vec<Vec<ScannedFile>>) -> Vec<Vec<ScannedFile>> {
    for g in &mut groups {
        g.sort_by(|a, b| a.path.cmp(&b.path));
    }
    groups.sort_by(|a, b| a[0].path.cmp(&b[0].path));
    groups
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DuplicateMode {
    KeepOldest,
    KeepNewest,
    DeleteAll,
}

pub fn to_delete(groups: &[Vec<ScannedFile>], mode: DuplicateMode) -> Vec<ScannedFile> {
    let mut doomed = Vec::new();
    for g in groups {
        match mode {
            DuplicateMode::DeleteAll => doomed.extend(g.iter().cloned()),
            DuplicateMode::KeepOldest | DuplicateMode::KeepNewest => {
                // First wins on ties, so the choice is deterministic (groups are path-sorted).
                let keep = if mode == DuplicateMode::KeepOldest {
                    g.iter().enumerate().min_by_key(|(i, f)| (f.modified, *i))
                } else {
                    g.iter()
                        .enumerate()
                        .max_by_key(|(i, f)| (f.modified, std::cmp::Reverse(*i)))
                };
                if let Some((k, _)) = keep {
                    doomed.extend(
                        g.iter()
                            .enumerate()
                            .filter(|(i, _)| *i != k)
                            .map(|(_, f)| f.clone()),
                    );
                }
            }
        }
    }
    doomed
}

// ------------------------------------------------------------ tree

fn sorted_names(mut names: Vec<String>) -> Vec<String> {
    names.sort_by(|a, b| {
        a.to_lowercase()
            .cmp(&b.to_lowercase())
            .then_with(|| a.cmp(b))
    });
    names
}

pub fn render_tree(
    roots: &[PathBuf],
    settings: &TreeSettings,
    progress: &dyn Progress,
) -> Result<String, Cancelled> {
    let excl = ExclusionFilter::from_rules(&settings.exclusions);
    let mut text = String::new();
    let mut budget = TREE_MAX_ENTRIES;
    for root in roots {
        if progress.cancelled() {
            return Err(Cancelled);
        }
        if !root.symlink_metadata().map(|m| m.is_dir()).unwrap_or(false) {
            continue;
        }
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(&root.to_string_lossy());
        text.push('\n');
        branch(root, "", &mut text, settings, &excl, progress, &mut budget)?;
    }
    Ok(text)
}

fn children(dir: &Path, excl: &ExclusionFilter) -> (Vec<String>, Vec<String>) {
    let (mut folders, mut files) = (Vec::new(), Vec::new());
    if let Ok(entries) = fs::read_dir(dir) {
        for e in entries.flatten() {
            let Ok(meta) = e.path().symlink_metadata() else {
                continue;
            };
            let name = e.file_name().to_string_lossy().into_owned();
            let ft = meta.file_type();
            if ft.is_dir() && !excl.excludes_folder(&name) {
                folders.push(name);
            } else if !ft.is_dir() && !ft.is_symlink() && !excl.excludes_file(&name) {
                files.push(name);
            }
        }
    }
    (sorted_names(folders), sorted_names(files))
}

fn branch(
    dir: &Path,
    prefix: &str,
    text: &mut String,
    s: &TreeSettings,
    excl: &ExclusionFilter,
    progress: &dyn Progress,
    budget: &mut usize,
) -> Result<(), Cancelled> {
    if progress.cancelled() {
        return Err(Cancelled);
    }
    let (folders, mut files) = children(dir, excl);
    if s.folders_only {
        files.clear();
    }
    for (i, f) in folders.iter().enumerate() {
        if *budget == 0 {
            return Ok(());
        }
        *budget -= 1;
        let last = i == folders.len() - 1 && files.is_empty();
        text.push_str(&format!(
            "{prefix}{}{f}\n",
            if last { "└───" } else { "├───" }
        ));
        let next = format!("{prefix}{}", if last { "    " } else { "│   " });
        branch(&dir.join(f), &next, text, s, excl, progress, budget)?;
    }
    for (i, f) in files.iter().enumerate() {
        if *budget == 0 {
            return Ok(());
        }
        *budget -= 1;
        text.push_str(&format!(
            "{prefix}{}{f}\n",
            if i == files.len() - 1 {
                "└───"
            } else {
                "├───"
            }
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn write(p: &Path, data: &[u8]) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, data).unwrap();
    }

    #[test]
    fn exclusion_patterns() {
        let f = ExclusionFilter::from_rules(&ExclusionRules {
            folders: "node_modules;.git".into(),
            files: "*.tmp\n~$*".into(),
            extensions: "log;bak".into(),
        });
        assert!(
            f.excludes_folder("NODE_MODULES")
                && f.excludes_folder(".git")
                && !f.excludes_folder("src")
        );
        assert!(
            f.excludes_file("a.TMP") && f.excludes_file("~$doc.docx") && !f.excludes_file("a.txt")
        );
        assert!(f.excludes_file("x.log") && f.excludes_file("y.BAK") && !f.excludes_file("log"));
        assert!(matches("a?c*", "abcdef") && !matches("a?c", "abcd"));
    }

    #[test]
    fn scan_finds_files_and_empty_dirs_and_skips_symlinks() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        write(&r.join("a/x.txt"), b"1");
        write(&r.join("only_litter/.DS_Store"), b"junk");
        fs::create_dir_all(r.join("empty/inner")).unwrap();
        std::os::unix::fs::symlink(r, r.join("a/loop")).unwrap(); // would recurse forever if followed
        std::os::unix::fs::symlink(r.join("a"), r.join("linkonly_parent_dir")).unwrap();
        let s = scan(
            &[r.to_path_buf()],
            &ExclusionFilter::default(),
            &EmptyFolderSettings::default(),
            &NullProgress,
        )
        .unwrap();
        assert_eq!(s.files.iter().filter(|f| f.extension == "txt").count(), 1);
        let mut empties: Vec<_> = s
            .empty_directories
            .iter()
            .map(|p| p.strip_prefix(r).unwrap().to_string_lossy().into_owned())
            .collect();
        empties.sort();
        assert_eq!(empties, ["empty", "empty/inner", "only_litter"]);
        assert_eq!(outermost(&s.empty_directories).len(), 2);
    }

    #[test]
    fn litter_counts_as_content_when_toggle_is_off() {
        let d = tempfile::tempdir().unwrap();
        write(&d.path().join("k/.DS_Store"), b"x");
        let empty = EmptyFolderSettings {
            ignore_desktop_ini: false,
            ..Default::default()
        };
        let s = scan(
            &[d.path().into()],
            &ExclusionFilter::default(),
            &empty,
            &NullProgress,
        )
        .unwrap();
        assert!(s.empty_directories.is_empty());
    }

    #[test]
    fn duplicates_are_confirmed_by_content() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        let big: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let mut big2 = big.clone();
        *big2.last_mut().unwrap() ^= 1; // same size and same 64K head, different tail
        write(&r.join("a/big1.bin"), &big);
        write(&r.join("b/big1_copy.bin"), &big);
        write(&r.join("c/big2.bin"), &big2);
        write(&r.join("a/small.txt"), b"hello");
        write(&r.join("b/small.txt"), b"hello");
        write(&r.join("a/empty1"), b"");
        write(&r.join("a/empty2"), b"");
        let s = scan(
            &[r.into()],
            &ExclusionFilter::default(),
            &EmptyFolderSettings::default(),
            &NullProgress,
        )
        .unwrap();
        let groups =
            find_duplicates(&s.files, &DuplicateSettings::default(), &NullProgress).unwrap();
        let names: Vec<Vec<String>> = groups
            .iter()
            .map(|g| {
                g.iter()
                    .map(|f| {
                        f.path
                            .strip_prefix(r)
                            .unwrap()
                            .to_string_lossy()
                            .into_owned()
                    })
                    .collect()
            })
            .collect();
        assert_eq!(
            names,
            [
                vec!["a/big1.bin", "b/big1_copy.bin"],
                vec!["a/small.txt", "b/small.txt"]
            ]
        );

        // HeadHash is the loose mode: the tail difference is missed on purpose.
        let loose = DuplicateSettings {
            strictness: DuplicateStrictness::HeadHash,
            ..Default::default()
        };
        assert_eq!(
            find_duplicates(&s.files, &loose, &NullProgress)
                .unwrap()
                .iter()
                .flatten()
                .filter(|f| f.size > 100_000)
                .count(),
            3
        );
        // SizeOnly + empty files not ignored
        let size = DuplicateSettings {
            strictness: DuplicateStrictness::SizeOnly,
            ignore_empty_files: false,
            ..Default::default()
        };
        assert!(find_duplicates(&s.files, &size, &NullProgress)
            .unwrap()
            .iter()
            .any(|g| g[0].size == 0));
    }

    #[test]
    fn same_content_detects_difference_anywhere() {
        let d = tempfile::tempdir().unwrap();
        let (a, b) = (d.path().join("a"), d.path().join("b"));
        write(&a, &vec![7u8; 70_000]);
        let mut other = vec![7u8; 70_000];
        other[69_999] = 8;
        write(&b, &other);
        assert!(!same_content(&a, &b).unwrap());
        assert!(same_content(&a, &a).unwrap());
    }

    #[test]
    fn keep_policy_is_deterministic() {
        let t = |s| SystemTime::UNIX_EPOCH + Duration::from_secs(s);
        let f = |n: &str, s| ScannedFile {
            path: n.into(),
            size: 1,
            modified: t(s),
            extension: String::new(),
        };
        let groups = vec![vec![f("a", 20), f("b", 10), f("c", 30)]];
        let names = |v: Vec<ScannedFile>| {
            v.into_iter()
                .map(|f| f.path.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(to_delete(&groups, DuplicateMode::KeepOldest)),
            ["a", "c"]
        );
        assert_eq!(
            names(to_delete(&groups, DuplicateMode::KeepNewest)),
            ["a", "b"]
        );
        assert_eq!(to_delete(&groups, DuplicateMode::DeleteAll).len(), 3);
        let ties = vec![vec![f("a", 5), f("b", 5)]];
        assert_eq!(names(to_delete(&ties, DuplicateMode::KeepOldest)), ["b"]);
        assert_eq!(names(to_delete(&ties, DuplicateMode::KeepNewest)), ["b"]);
    }

    #[test]
    fn tree_layout() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        write(&r.join("src/b.rs"), b"");
        write(&r.join("src/A.rs"), b"");
        write(&r.join("README.md"), b"");
        fs::create_dir(r.join("docs")).unwrap();
        let out = render_tree(&[r.into()], &TreeSettings::default(), &NullProgress).unwrap();
        let expected = format!(
            "{}\n├───docs\n├───src\n│   ├───A.rs\n│   └───b.rs\n└───README.md\n",
            r.display()
        );
        assert_eq!(out, expected);
        let only = render_tree(
            &[r.into()],
            &TreeSettings {
                folders_only: true,
                ..Default::default()
            },
            &NullProgress,
        )
        .unwrap();
        assert_eq!(only, format!("{}\n├───docs\n└───src\n", r.display()));
    }

    #[test]
    fn cancellation_is_honoured() {
        struct Stop;
        impl Progress for Stop {
            fn cancelled(&self) -> bool {
                true
            }
        }
        let d = tempfile::tempdir().unwrap();
        assert_eq!(
            scan(
                &[d.path().into()],
                &ExclusionFilter::default(),
                &EmptyFolderSettings::default(),
                &Stop
            )
            .err(),
            Some(Cancelled)
        );
    }

    #[test]
    fn unicode_names_and_extension_rules() {
        assert_eq!(extension_of("写真.JPG"), "jpg");
        assert_eq!(extension_of(".hidden"), "");
        assert_eq!(extension_of("noext"), "");
    }
}
