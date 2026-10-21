//! Folder tasks: exclusion rules, scanning, duplicate detection, empty folders, tree.
//! Symlinks are never followed (no loops, no escaping the selected tree).
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rayon::prelude::*;
use xxhash_rust::xxh3::Xxh3;

use crate::fsat;

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
    /// (device, inode) as reported by the file system. Two different files on some file systems
    /// can report the same pair, so listing uses [`ScannedFile::same_file_key`]; but whatever decides
    /// what to delete treats a matching pair alone as one file ([`ScannedFile::may_be_same_file`]).
    pub file_id: (u64, u64),
    /// Status change time (ctime) as (seconds, nanoseconds): with the other fields, what a later
    /// look must find again before anything is removed ([`ScannedFile::unchanged`]).
    pub changed: (i64, i64),
    /// Modification time as (seconds, nanoseconds), as `stat` reports it.
    pub mtime: (i64, i64),
    /// (device, inode) of the folder the file was listed in.
    pub parent_id: (u64, u64),
    /// The selected folder the scan reached it from.
    pub root: std::sync::Arc<ScanRoot>,
}

/// A selected folder as scanned: the path it was given as, and its (device, inode) then.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScanRoot {
    pub path: PathBuf,
    pub id: (u64, u64),
}

/// What two entries must share to be listed once (see [`TreeScan`]).
pub type SameFileKey = (u64, u64, u64, SystemTime);

impl ScannedFile {
    /// Device, inode, size and modification time: the key for *listing* only. Hard links share all
    /// four; two different files on a file system that reports the same inode number for both
    /// almost never do, so neither hides the other in the scan or the counts.
    pub fn same_file_key(&self) -> SameFileKey {
        (self.file_id.0, self.file_id.1, self.size, self.modified)
    }

    /// Whether the two entries may be one file: same (device, inode), whatever their size or time.
    /// One file reached twice can differ in those (a firmlink spelling such as
    /// `/System/Volumes/Data/Users/...` visited again after the file was touched), so deciding
    /// what to delete uses this: of such a pair, neither name is ever removed.
    pub fn may_be_same_file(&self, other: &ScannedFile) -> bool {
        self.file_id == other.file_id
    }

    /// Whether `m` (taken without following links) still describes the file the scan saw: a regular
    /// file with the same device, inode, size, modification time and status change time (to the
    /// nanosecond). A file rewritten in place, replaced by another one (an atomic save) or recreated
    /// under a reused inode number (FAT) fails at least one of these.
    pub fn unchanged(&self, m: &fs::Metadata) -> bool {
        m.file_type().is_file() && fsat::Fingerprint::of_metadata(m) == self.fingerprint()
    }

    /// Device, inode, size, modification and status change time, as recorded by the scan.
    pub fn fingerprint(&self) -> fsat::Fingerprint {
        fsat::Fingerprint {
            dev: self.file_id.0,
            ino: self.file_id.1,
            size: self.size,
            mtime: self.mtime,
            ctime: self.changed,
        }
    }
}

#[derive(Debug, Default)]
pub struct TreeScan {
    pub files: Vec<ScannedFile>,
    pub empty_directories: Vec<PathBuf>,
    /// (device, inode) of each folder in `empty_directories`, as scanned.
    pub dir_ids: HashMap<PathBuf, (u64, u64)>,
    /// Each selected folder scanned, with its (device, inode) then.
    pub root_ids: Vec<(PathBuf, (u64, u64))>,
    pub directory_count: usize,
    /// Files already listed, so a hard link (a second name for the same file) is listed once.
    seen: HashSet<SameFileKey>,
}

impl TreeScan {
    /// List `f` unless it is another name for a file already listed (same device, inode, size and
    /// modification time). Returns whether it was listed.
    fn admit(&mut self, f: ScannedFile) -> bool {
        if !self.seen.insert(f.same_file_key()) {
            return false;
        }
        self.files.push(f);
        true
    }

    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

/// Files that do not make a folder "non-empty" (OS litter), per the user's toggles.
/// The Windows toggle names are kept for settings compatibility; the "desktop.ini"
/// toggle also covers macOS litter (`.DS_Store`, `.localized`, `Icon\r`, AppleDouble `._*`).
/// The opt-in zero-byte toggle (off by default) is the Windows original's `IgnoreZeroByteFiles`:
/// a 0-byte file is ignored only in the sense that it alone does not make its folder non-empty;
/// anything else in the folder (a file, an excluded or unreadable folder, a link) still does.
/// Only regular files reach this function; folders, links and special files are always content.
pub(crate) fn ignorable(name: &str, size: u64, empty: &EmptyFolderSettings) -> bool {
    if empty.ignore_zero_byte_files && size == 0 {
        return true;
    }
    let lower = name.to_lowercase();
    if empty.ignore_desktop_ini
        && (lower == "desktop.ini"
            || lower == ".ds_store"
            || lower == ".localized"
            || name == "Icon\r"
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

/// The selection without repeats and without folders that lie inside another selected folder
/// (compared after resolving the path, so `/var/x` and `/private/var/x` are the same folder).
/// Order is kept; of two spellings of one folder the first wins. Scanning a folder twice would list
/// every file in it twice, and the duplicate finder would then offer a file as its own copy.
///
/// O(n log n): the resolved paths are sorted component-wise (`Path`'s order), so a folder comes
/// right before everything inside it, and a root is dropped when the last root kept is one of its
/// ancestors (or itself). `Path::starts_with` compares whole components: `/a/bc` is not in `/a/b`.
/// Resolving can take a while for thousands of roots, so the cancel flag is checked as it goes.
pub fn distinct_roots(
    roots: &[PathBuf],
    progress: &dyn Progress,
) -> Result<Vec<PathBuf>, Cancelled> {
    let mut canon: Vec<(PathBuf, usize)> = Vec::with_capacity(roots.len());
    for (i, r) in roots.iter().enumerate() {
        if i % 256 == 0 && progress.cancelled() {
            return Err(Cancelled);
        }
        canon.push((fs::canonicalize(r).unwrap_or_else(|_| r.clone()), i));
    }
    // Equal paths sort by original position, so the first spelling is the one kept.
    canon.sort_unstable();
    let mut keep = vec![false; roots.len()];
    let mut last: Option<&Path> = None;
    for (path, i) in &canon {
        if last.is_some_and(|kept| path.starts_with(kept)) {
            continue;
        }
        keep[*i] = true;
        last = Some(path);
    }
    // The same folder can have spellings that resolving does not unify: a firmlink
    // (`/System/Volumes/Data/Users/x` is `/Users/x`). Compare the folders themselves by
    // (device, inode): a root goes when it is a root already kept, or lies inside one.
    let mut kept_ids: HashMap<(u64, u64), usize> = HashMap::new();
    for (i, r) in roots.iter().enumerate() {
        if keep[i] {
            if let Some(d) = dir_id(r) {
                kept_ids.entry(d).or_insert(i);
            }
        }
    }
    for (n, (path, i)) in canon.iter().enumerate() {
        if n % 256 == 0 && progress.cancelled() {
            return Err(Cancelled);
        }
        if !keep[*i] {
            continue;
        }
        let covered = path.ancestors().enumerate().any(|(depth, a)| {
            dir_id(a).and_then(|d| kept_ids.get(&d)).is_some_and(|&j| {
                if depth == 0 {
                    j < *i
                } else {
                    j != *i
                }
            })
        });
        if covered {
            keep[*i] = false;
        }
    }
    if progress.cancelled() {
        return Err(Cancelled);
    }
    Ok(roots
        .iter()
        .zip(keep)
        .filter(|(_, k)| *k)
        .map(|(r, _)| r.clone())
        .collect())
}

/// (device, inode) of a folder, links not followed.
pub(crate) fn dir_id(p: &Path) -> Option<(u64, u64)> {
    fs::symlink_metadata(p)
        .ok()
        .filter(|m| m.is_dir())
        .map(|m| (m.dev(), m.ino()))
}

/// Walk the (de-duplicated, see [`distinct_roots`]) selection. Every file is listed once: a file
/// reached under two names (hard links) is one file, listed under the first name met.
pub fn scan(
    roots: &[PathBuf],
    excl: &ExclusionFilter,
    empty: &EmptyFolderSettings,
    progress: &dyn Progress,
) -> Result<TreeScan, Cancelled> {
    let mut out = TreeScan::default();
    for root in &distinct_roots(roots, progress)? {
        if progress.cancelled() {
            return Err(Cancelled);
        }
        if let Some(id) = dir_id(root) {
            out.root_ids.push((root.clone(), id));
            let origin = std::sync::Arc::new(ScanRoot {
                path: root.clone(),
                id,
            });
            walk(root, &origin, &mut out, excl, empty, progress, true)?;
        }
    }
    Ok(out)
}

/// Returns whether the directory contains something that counts as content. Anything the walk
/// cannot see into counts as content: an unreadable folder or entry, and an excluded folder (it is
/// not looked at, so its parent is never "empty").
fn walk(
    dir: &Path,
    origin: &std::sync::Arc<ScanRoot>,
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
       // (0, 0) never matches a real folder: files under an unidentifiable folder are never removed.
    let parent_id = dir_id(dir).unwrap_or((0, 0));
    let mut has_content = false;
    let mut subdirs = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else {
            has_content = true; // unreadable entry
            continue;
        };
        let Ok(meta) = entry.path().symlink_metadata() else {
            has_content = true; // vanished or unreadable: never assume it was nothing
            continue;
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        let ft = meta.file_type();
        if ft.is_symlink() {
            has_content = true; // a link is content, but is never followed or listed
            continue;
        }
        if ft.is_dir() {
            if excl.excludes_folder(&name) {
                has_content = true; // not scanned, so not known to be empty
            } else {
                subdirs.push(entry.path());
            }
        } else if ft.is_file() {
            if !ignorable(&name, meta.len(), empty) {
                has_content = true;
            }
            if excl.excludes_file(&name) {
                continue;
            }
            let listed = out.admit(ScannedFile {
                path: entry.path(),
                size: meta.len(),
                modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                extension: extension_of(&name),
                file_id: (meta.dev(), meta.ino()),
                changed: (meta.ctime(), meta.ctime_nsec()),
                mtime: (meta.mtime(), meta.mtime_nsec()),
                parent_id,
                root: origin.clone(),
            });
            if !listed {
                continue; // another name for a file already listed
            }
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
        if walk(&sub, origin, out, excl, empty, progress, false)? {
            has_content = true;
        }
    }
    if !has_content && !is_root {
        out.empty_directories.push(dir.to_path_buf());
        out.dir_ids.insert(dir.to_path_buf(), parent_id);
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

/// OS bookkeeping files that belong to another file or folder, never duplicates of anything:
/// AppleDouble `._*` (on FAT, exFAT and SMB they hold the extended attributes and resource fork of
/// the file next to them), `.DS_Store`, `.localized`, `Icon\r`, `desktop.ini`, `Thumbs.db`.
pub fn is_os_litter(name: &str) -> bool {
    let lower = name.to_lowercase();
    name.starts_with("._")
        || name == "Icon\r"
        || matches!(
            lower.as_str(),
            ".ds_store" | ".localized" | "desktop.ini" | "thumbs.db"
        )
}

/// Groups of identical files. OS litter ([`is_os_litter`]) is never considered: removing an
/// AppleDouble file as a "duplicate" strips its owner's metadata.
pub fn find_duplicates(
    files: &[ScannedFile],
    s: &DuplicateSettings,
    progress: &dyn Progress,
) -> Result<Vec<Vec<ScannedFile>>, Cancelled> {
    let eligible: Vec<&ScannedFile> = files
        .iter()
        .filter(|f| {
            !f.path
                .file_name()
                .is_some_and(|n| is_os_litter(&n.to_string_lossy()))
        })
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

/// Whether a group may list one file twice (same path, or same device and inode under another
/// name, whatever the size or time).
fn repeats_a_file(g: &[ScannedFile]) -> bool {
    let mut paths = HashSet::new();
    let mut ids = HashSet::new();
    g.iter()
        .any(|f| !paths.insert(f.path.as_path()) || !ids.insert(f.file_id))
}

fn sorted(groups: Vec<Vec<ScannedFile>>) -> Vec<Vec<ScannedFile>> {
    // Defence in depth (the scan already lists each file once): a group only ever holds distinct
    // files, and one that would shrink below two members is not a duplicate group at all.
    let mut groups: Vec<Vec<ScannedFile>> = groups
        .into_iter()
        .map(|mut g| {
            let (mut paths, mut ids) = (HashSet::new(), HashSet::new());
            g.retain(|f| paths.insert(f.path.clone()) & ids.insert(f.file_id));
            g
        })
        .filter(|g| g.len() > 1)
        .collect();
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

/// What goes for each group under `mode`. A group that lists one file twice (by path or inode) is
/// never acted on, and the copy that stays is never also in the list of copies that go.
pub fn to_delete(groups: &[Vec<ScannedFile>], mode: DuplicateMode) -> Vec<ScannedFile> {
    let mut doomed = Vec::new();
    for g in groups {
        if g.len() < 2 || repeats_a_file(g) {
            continue;
        }
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
                if let Some((k, kept)) = keep {
                    doomed.extend(
                        g.iter()
                            .enumerate()
                            .filter(|(i, f)| {
                                *i != k && f.path != kept.path && !f.may_be_same_file(kept)
                            })
                            .map(|(_, f)| f.clone()),
                    );
                }
            }
        }
    }
    doomed
}

/// The last check before a duplicate plan is shown or carried out: no file is listed for removal
/// twice (by path, or by device and inode), nothing listed for removal shares its device and inode
/// with anything that stays (in any group), and in a keep mode every group still keeps a copy.
pub fn plan_is_consistent(
    groups: &[Vec<ScannedFile>],
    doomed: &[ScannedFile],
    mode: DuplicateMode,
) -> bool {
    let (mut paths, mut ids) = (HashSet::new(), HashSet::new());
    if !doomed
        .iter()
        .all(|d| paths.insert(d.path.as_path()) & ids.insert(d.file_id))
    {
        return false;
    }
    let stays = |f: &ScannedFile| !paths.contains(f.path.as_path());
    if groups
        .iter()
        .flatten()
        .any(|f| stays(f) && ids.contains(&f.file_id))
    {
        return false; // one name of a file would go while another name of it stays
    }
    mode == DuplicateMode::DeleteAll || groups.iter().all(|g| g.is_empty() || g.iter().any(stays))
}

/// Right before a duplicate plan is shown or carried out, look again (without following links):
/// every entry of every group, kept or not, is still the file the scan saw
/// ([`ScannedFile::unchanged`]), and no copy that goes shares its (device, inode) with one that stays.
pub fn plan_still_holds(groups: &[Vec<ScannedFile>], doomed: &[ScannedFile]) -> bool {
    let same = |f: &ScannedFile| fs::symlink_metadata(&f.path).is_ok_and(|m| f.unchanged(&m));
    let gone: HashSet<&Path> = doomed.iter().map(|d| d.path.as_path()).collect();
    let kept_ids: HashSet<(u64, u64)> = groups
        .iter()
        .flatten()
        .filter(|f| !gone.contains(f.path.as_path()))
        .map(|f| f.file_id)
        .collect();
    groups.iter().flatten().all(same)
        && doomed
            .iter()
            .all(|d| same(d) && !kept_ids.contains(&d.file_id))
}

/// The groups whose every entry is still the file the scan saw, and the paths of the entries that
/// changed (their groups are left alone).
pub fn unchanged_groups(groups: Vec<Vec<ScannedFile>>) -> (Vec<Vec<ScannedFile>>, Vec<PathBuf>) {
    let mut changed = Vec::new();
    let held = groups
        .into_iter()
        .filter(|g| {
            let moved: Vec<PathBuf> = g
                .iter()
                .filter(|f| !fs::symlink_metadata(&f.path).is_ok_and(|m| f.unchanged(&m)))
                .map(|f| f.path.clone())
                .collect();
            let ok = moved.is_empty();
            changed.extend(moved);
            ok
        })
        .collect();
    (held, changed)
}

/// What became of one duplicate at the moment it was to be removed.
#[derive(Debug)]
pub enum Removal {
    /// Removed (permanent mode).
    Removed,
    /// Every check passed; the caller hands it on to the Trash (Trash mode).
    Verified,
    /// It, or the copy that stays, is not what was scanned and compared: left alone.
    Changed,
    /// The removal itself failed.
    Failed(io::Error),
    /// Not what was checked any more, and its name was taken in the meantime: it was left in its
    /// folder under this temporary name (nothing was removed).
    LeftAs(PathBuf),
}

/// Folders already opened and verified during one removal run, so files in the same (deep)
/// folder do not walk the way from the selected folder again and again. Bounded: at most
/// [`ParentCache::CAP`] descriptors are kept. A cached descriptor always refers to the folder that
/// was verified when it was opened (a descriptor follows its folder); whether that folder is still
/// where its path leads is checked separately, right before each removal.
#[derive(Default)]
pub struct ParentCache {
    open: std::cell::RefCell<(OpenFolders, std::collections::VecDeque<PathBuf>)>,
}

/// Folder path -> its verified descriptor and (device, inode).
type OpenFolders = HashMap<PathBuf, (std::rc::Rc<std::os::fd::OwnedFd>, (u64, u64))>;

impl ParentCache {
    pub const CAP: usize = 32;
}

type Parent = (std::rc::Rc<std::os::fd::OwnedFd>, std::ffi::OsString);

/// The folder a scanned file was listed in, opened through handles from its selected folder
/// ([`fsat::open_below`]: the root checked by (device, inode), every folder below it opened
/// without following links), and checked to be that same folder; plus the file's name in it.
fn open_parent(f: &ScannedFile, cache: &ParentCache) -> Option<Parent> {
    let rel = f.path.strip_prefix(&f.root.path).ok()?;
    let name = rel.file_name()?.to_owned();
    let key = f.path.parent()?.to_path_buf();
    let mut open = cache.open.borrow_mut();
    if let Some((fd, id)) = open.0.get(&key) {
        if *id == f.parent_id {
            return Some((fd.clone(), name));
        }
    }
    let fd = fsat::open_below(
        &f.root.path,
        Some(f.root.id),
        rel.parent().unwrap_or(Path::new("")),
    )
    .ok()?;
    let id = fsat::Fingerprint::of_stat(&fsat::stat_fd(&fd).ok()?).id();
    if id != f.parent_id {
        return None;
    }
    let fd = std::rc::Rc::new(fd);
    if open.0.insert(key.clone(), (fd.clone(), id)).is_none() {
        open.1.push_back(key);
        while open.1.len() > ParentCache::CAP {
            if let Some(old) = open.1.pop_front() {
                open.0.remove(&old);
            }
        }
    }
    Some((fd, name))
}

/// Whether the folder `f` was listed in is still where its path leads (`lstat` of the folder's
/// path: a link put in its place, or another folder moved there, has another identity).
fn parent_still_here(f: &ScannedFile) -> bool {
    f.path.parent().and_then(dir_id) == Some(f.parent_id)
}

/// Whether `name` in the open folder `dir` is still the regular file `f` describes (not followed).
fn is_still(f: &ScannedFile, dir: &std::os::fd::OwnedFd, name: &std::ffi::OsStr) -> bool {
    fsat::stat_at(dir, name)
        .is_ok_and(|st| fsat::is_file(&st) && fsat::Fingerprint::of_stat(&st) == f.fingerprint())
}

fn open_still(f: &ScannedFile, dir: &std::os::fd::OwnedFd, name: &std::ffi::OsStr) -> Option<File> {
    let file = fsat::open_file_at(dir, name).ok()?;
    file.metadata()
        .is_ok_and(|m| f.unchanged(&m))
        .then_some(file)
}

/// Remove one duplicate (permanent mode, `unlink`) or confirm it may go to the Trash, right now:
/// 1. the folders of `doomed` and of `kept` (the copy that stays, in a keep mode) are opened
///    through handles from their selected folder and must be the folders the scan listed them in;
/// 2. both files, looked at in those folders without following links and through descriptors
///    opened there, must still have the scanned device, inode, size and times;
/// 3. their bytes must still be identical (streamed, bounded memory, cancellable), and step 2
///    must still hold afterwards;
/// 4. right before the removal the folder must still be where the path leads; the file is then
///    removed through the verified folder with [`fsat::remove_checked`]: renamed to a fresh
///    temporary name (never replacing anything), checked again under that name (device, inode,
///    size and modification time; the rename itself changes the status change time), and only
///    then removed with `unlinkat`. Never by path, so renaming or swapping a folder above it, or
///    another file renamed into its name, cannot make it remove another file.
///
/// `before_unlink` runs between the checks and step 4 (a test seam; a no-op otherwise).
pub fn remove_duplicate(
    doomed: &ScannedFile,
    kept: Option<&ScannedFile>,
    unlink: bool,
    progress: &dyn Progress,
    cache: &ParentCache,
    before_unlink: &dyn Fn(&ScannedFile),
) -> Result<Removal, Cancelled> {
    let Some((dp, dn)) = open_parent(doomed, cache) else {
        return Ok(Removal::Changed);
    };
    fsat::seam("D1", &doomed.path);
    let Some(mut d) = open_still(doomed, &dp, &dn) else {
        return Ok(Removal::Changed);
    };
    if let Some(kept) = kept {
        if kept.may_be_same_file(doomed) || kept.path == doomed.path {
            return Ok(Removal::Changed);
        }
        let Some((kp, kn)) = open_parent(kept, cache) else {
            return Ok(Removal::Changed);
        };
        let Some(mut k) = open_still(kept, &kp, &kn) else {
            return Ok(Removal::Changed);
        };
        let (mut bd, mut bk) = (vec![0u8; 64 * 1024], vec![0u8; 64 * 1024]);
        loop {
            if progress.cancelled() {
                return Err(Cancelled);
            }
            let (Ok(nd), Ok(nk)) = (read_full(&mut d, &mut bd), read_full(&mut k, &mut bk)) else {
                return Ok(Removal::Changed);
            };
            if nd != nk || bd[..nd] != bk[..nk] {
                return Ok(Removal::Changed);
            }
            if nd == 0 {
                break;
            }
        }
        fsat::seam("D2", &doomed.path);
        let still = |f: &ScannedFile, file: &File| file.metadata().is_ok_and(|m| f.unchanged(&m));
        if !(still(kept, &k) && is_still(kept, &kp, &kn)) {
            return Ok(Removal::Changed);
        }
    }
    if !(d.metadata().is_ok_and(|m| doomed.unchanged(&m)) && is_still(doomed, &dp, &dn)) {
        return Ok(Removal::Changed);
    }
    before_unlink(doomed);
    fsat::seam("D3", &doomed.path);
    // The folder must still be where its path leads (nothing above it renamed or swapped), and
    // the name in it must still be the file checked.
    if !parent_still_here(doomed) || !is_still(doomed, &dp, &dn) {
        return Ok(Removal::Changed);
    }
    fsat::seam("D4", &doomed.path);
    if !unlink {
        return Ok(Removal::Verified);
    }
    let want = doomed.fingerprint();
    let same = |st: &rustix::fs::Stat| {
        let now = fsat::Fingerprint::of_stat(st);
        (now.dev, now.ino, now.size, now.mtime) == (want.dev, want.ino, want.size, want.mtime)
    };
    Ok(
        match fsat::remove_checked(&dp, &dn, false, &same, &|| true) {
            Ok(()) => Removal::Removed,
            Err(fsat::NotRemoved::Gone | fsat::NotRemoved::Changed) => Removal::Changed,
            Err(fsat::NotRemoved::LeftAs(t)) => Removal::LeftAs(doomed.path.with_file_name(t)),
            Err(fsat::NotRemoved::Failed(e)) => Removal::Failed(e),
        },
    )
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
            file_id: (1, u64::from(n.as_bytes()[0])),
            changed: (0, 0),
            mtime: (0, 0),
            parent_id: (0, 0),
            root: Default::default(),
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
        // A group that names one file twice (same path, or same inode under another name) is
        // never acted on in any mode: removing "the other copy" would remove the only one.
        let same_path = vec![vec![f("a", 5), f("a", 6)]];
        let mut alias = f("z", 5);
        alias.file_id = f("a", 5).file_id; // a hard link: same device, inode, size and time
        let same_inode = vec![vec![f("a", 5), alias]];
        for g in [&same_path, &same_inode] {
            for m in [
                DuplicateMode::KeepOldest,
                DuplicateMode::KeepNewest,
                DuplicateMode::DeleteAll,
            ] {
                assert!(to_delete(g, m).is_empty(), "{m:?} {g:?}");
            }
        }
        assert!(sorted(same_path.clone()).is_empty() && sorted(same_inode.clone()).is_empty());
    }

    /// Some file systems report one inode number for different files: listing keeps them apart
    /// unless device, inode, size and time all agree (hard links). Deciding what to delete never
    /// separates a matching (device, inode): one file seen twice through a firmlink can differ in
    /// time (touched between the two visits), and removing "the other copy" removed the only one.
    #[test]
    fn same_inode_lists_apart_but_is_never_deleted_apart() {
        let t = |s| SystemTime::UNIX_EPOCH + Duration::from_secs(s);
        let f = |n: &str, size, s| ScannedFile {
            path: n.into(),
            size,
            modified: t(s),
            extension: String::new(),
            file_id: (7, 42),
            changed: (0, 0),
            mtime: (0, 0),
            parent_id: (0, 0),
            root: Default::default(),
        };
        let (a, link) = (f("a", 10, 100), f("hardlink", 10, 100));
        let (other_size, other_time) = (f("b", 11, 100), f("c", 10, 101));
        assert!(a.same_file_key() == link.same_file_key() && a.may_be_same_file(&other_time));
        assert!(
            a.same_file_key() != other_size.same_file_key()
                && a.same_file_key() != other_time.same_file_key()
        );
        let mut other_dev = f("d", 10, 100);
        other_dev.file_id = (8, 42);
        assert!(a.same_file_key() != other_dev.same_file_key() && !a.may_be_same_file(&other_dev));

        // scan: the hard link is dropped, the inode-sharing strangers are all listed
        let mut scan = TreeScan::default();
        for e in [&a, &link, &other_size, &other_time, &other_dev] {
            scan.admit(e.clone());
        }
        let listed: Vec<_> = scan
            .files
            .iter()
            .map(|f| f.path.to_str().unwrap())
            .collect();
        assert_eq!(listed, ["a", "b", "c", "d"]);

        // the firmlink case: same (device, inode), another time; neither name ever goes
        let g = |x: &ScannedFile, y: &ScannedFile| vec![vec![x.clone(), y.clone()]];
        let (data, short) = (f("/System/Volumes/Data/T/x", 10, 100), f("/T/x", 10, 200));
        let firmlink = g(&data, &short);
        assert!(repeats_a_file(&firmlink[0]));
        assert!(sorted(firmlink.clone()).is_empty(), "not a duplicate group");
        for m in [
            DuplicateMode::KeepOldest,
            DuplicateMode::KeepNewest,
            DuplicateMode::DeleteAll,
        ] {
            assert!(to_delete(&firmlink, m).is_empty(), "{m:?}");
        }
        assert!(!plan_is_consistent(
            &firmlink,
            std::slice::from_ref(&data),
            DuplicateMode::KeepNewest
        ));
        // the pair in one group with a real copy: the group is not acted on at all
        let mut copy = f("/T/copy", 10, 50);
        copy.file_id = (7, 99);
        let mixed = vec![vec![data.clone(), short.clone(), copy.clone()]];
        assert!(to_delete(&mixed, DuplicateMode::KeepNewest).is_empty());
        // the two names in two groups: one name may not go while the other stays
        let mut other = f("/T/other", 10, 300);
        other.file_id = (7, 100);
        let split = vec![vec![short.clone(), copy.clone()], vec![data.clone(), other]];
        assert!(!plan_is_consistent(
            &split,
            std::slice::from_ref(&data),
            DuplicateMode::KeepNewest
        ));
        assert!(plan_is_consistent(
            &split,
            &[copy.clone()],
            DuplicateMode::KeepNewest
        ));
        // ...and a group naming one file twice (a hard link) is never acted on either
        let linked = g(&a, &link);
        assert!(repeats_a_file(&linked[0]) && sorted(linked.clone()).is_empty());
        for m in [
            DuplicateMode::KeepOldest,
            DuplicateMode::KeepNewest,
            DuplicateMode::DeleteAll,
        ] {
            assert!(to_delete(&linked, m).is_empty(), "{m:?}");
        }
        assert!(!plan_is_consistent(
            &linked,
            &[a.clone(), link.clone()],
            DuplicateMode::DeleteAll
        ));
    }

    fn safe(d: &ScannedFile, k: Option<&ScannedFile>, p: &dyn Progress) -> Result<bool, Cancelled> {
        remove_duplicate(d, k, false, p, &ParentCache::default(), &|_| {})
            .map(|r| matches!(r, Removal::Verified))
    }

    /// Files in one (deep) folder reuse its verified descriptor instead of walking the way from
    /// the selected folder again; the cache is bounded.
    #[test]
    fn verified_parent_folders_are_cached_and_bounded() {
        let d = tempfile::tempdir().unwrap();
        let mut deep = d.path().to_path_buf();
        for _ in 0..150 {
            deep.push("d");
        }
        for i in 0..3 {
            write(&deep.join(format!("f{i}")), b"x");
        }
        for i in 0..(ParentCache::CAP + 5) {
            write(&d.path().join(format!("w{i}/f")), b"y");
        }
        let s = scan(
            &[d.path().to_path_buf()],
            &ExclusionFilter::default(),
            &EmptyFolderSettings::default(),
            &NullProgress,
        )
        .unwrap();
        let cache = ParentCache::default();
        let in_deep: Vec<_> = s
            .files
            .iter()
            .filter(|f| f.path.starts_with(&deep))
            .collect();
        assert_eq!(in_deep.len(), 3);
        let a = open_parent(in_deep[0], &cache).unwrap();
        let b = open_parent(in_deep[1], &cache).unwrap();
        assert!(
            std::rc::Rc::ptr_eq(&a.0, &b.0),
            "same folder, one descriptor"
        );
        for f in s.files.iter().filter(|f| !f.path.starts_with(&deep)) {
            assert!(open_parent(f, &cache).is_some());
        }
        assert!(cache.open.borrow().0.len() <= ParentCache::CAP);
        // a cached descriptor is only used for files listed in that same folder
        let mut stranger = in_deep[2].clone();
        stranger.parent_id = (0, 0);
        assert!(open_parent(&stranger, &cache).is_none());
    }

    /// The per-file check made right before a duplicate is removed.
    #[test]
    fn still_safe_to_remove_checks_identity_metadata_and_bytes() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        write(&r.join("k"), &vec![5u8; 200_000]);
        write(&r.join("x"), &vec![5u8; 200_000]);
        let rescan = || {
            let s = scan(
                &[r.to_path_buf()],
                &ExclusionFilter::default(),
                &EmptyFolderSettings::default(),
                &NullProgress,
            )
            .unwrap();
            let get = |n: &str| {
                s.files
                    .iter()
                    .find(|f| f.path == r.join(n))
                    .unwrap()
                    .clone()
            };
            (get("k"), get("x"))
        };
        let (k, x) = rescan();
        assert_ne!(x.changed, (0, 0));
        assert_eq!(safe(&x, Some(&k), &NullProgress), Ok(true));
        assert_eq!(safe(&x, None, &NullProgress), Ok(true));
        // the same file on both sides is never "safe"
        assert_eq!(safe(&x, Some(&x), &NullProgress), Ok(false));
        // any recorded field that no longer matches (as a reused FAT inode would show)
        for tweak in 0..4 {
            let mut y = x.clone();
            match tweak {
                0 => y.changed.1 += 1,
                1 => y.mtime.1 += 1,
                2 => y.size += 1,
                _ => y.file_id.1 += 1,
            }
            assert_eq!(safe(&y, Some(&k), &NullProgress), Ok(false), "{tweak}");
            assert_eq!(safe(&x, Some(&y), &NullProgress), Ok(false), "{tweak}");
        }
        // contents differing in the last byte, with metadata re-recorded: refused by the comparison
        let mut other = vec![5u8; 200_000];
        other[199_999] = 6;
        write(&r.join("x"), &other);
        let (k2, x2) = rescan();
        assert_eq!(safe(&x2, Some(&k2), &NullProgress), Ok(false));
        // a link where the file was: not followed, refused
        write(&r.join("x"), &vec![5u8; 200_000]);
        let (k3, x3) = rescan();
        fs::remove_file(r.join("x")).unwrap();
        std::os::unix::fs::symlink(r.join("k"), r.join("x")).unwrap();
        assert_eq!(safe(&x3, Some(&k3), &NullProgress), Ok(false));
        // cancelled during the comparison
        fs::remove_file(r.join("x")).unwrap();
        write(&r.join("x"), &vec![5u8; 200_000]);
        let (k4, x4) = rescan();
        struct Stop;
        impl Progress for Stop {
            fn cancelled(&self) -> bool {
                true
            }
        }
        assert_eq!(safe(&x4, Some(&k4), &Stop), Err(Cancelled));
        // plan_still_holds looks at every entry, kept ones included
        let groups = vec![vec![k4.clone(), x4.clone()]];
        assert!(plan_still_holds(&groups, std::slice::from_ref(&x4)));
        write(&r.join("k"), &vec![7u8; 200_000]);
        assert!(!plan_still_holds(&groups, std::slice::from_ref(&x4)));
        let (held, changed) = unchanged_groups(groups);
        assert!(held.is_empty());
        assert_eq!(changed, [r.join("k")]);
    }

    /// AppleDouble files of identical files are identical too; removing one strips its owner's
    /// extended attributes / resource fork (FAT, exFAT, SMB).
    #[test]
    fn os_litter_is_never_a_duplicate() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        for dir in ["a", "b"] {
            write(&r.join(dir).join("photo.jpg"), b"same picture");
            write(&r.join(dir).join("._photo.jpg"), b"same appledouble");
            for n in [
                ".DS_Store",
                ".localized",
                "Icon\r",
                "desktop.ini",
                "Thumbs.db",
                "._other",
            ] {
                write(&r.join(dir).join(n), b"same litter");
            }
        }
        let s = scan(
            &[r.to_path_buf()],
            &ExclusionFilter::default(),
            &EmptyFolderSettings::default(),
            &NullProgress,
        )
        .unwrap();
        for strictness in [
            DuplicateStrictness::SizeOnly,
            DuplicateStrictness::HeadHash,
            DuplicateSettings::default().strictness,
        ] {
            let settings = DuplicateSettings {
                strictness,
                ..Default::default()
            };
            let groups = find_duplicates(&s.files, &settings, &NullProgress).unwrap();
            let names: Vec<Vec<String>> = groups
                .iter()
                .map(|g| {
                    g.iter()
                        .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
                        .collect()
                })
                .collect();
            assert_eq!(names, [["photo.jpg", "photo.jpg"]], "{strictness:?}");
        }
        assert!(is_os_litter("._x") && is_os_litter(".ds_store") && is_os_litter("THUMBS.DB"));
        assert!(!is_os_litter("_x") && !is_os_litter("Icon") && !is_os_litter("notes.txt"));
    }

    #[test]
    fn nested_and_repeated_roots_are_scanned_once() {
        let d = tempfile::tempdir().unwrap();
        let (a, b) = (d.path().join("a"), d.path().join("ab"));
        write(&a.join("in/x.txt"), b"x");
        write(&b.join("y.txt"), b"y");
        fs::hard_link(a.join("in/x.txt"), a.join("x_link.txt")).unwrap();
        let canon = fs::canonicalize(&a).unwrap();
        assert_eq!(
            distinct_roots(
                &[a.clone(), a.join("in"), canon.clone(), b.clone()],
                &NullProgress
            )
            .unwrap(),
            [a.clone(), b.clone()],
            "`ab` is not inside `a`; `in` and the resolved spelling of `a` are"
        );
        assert_eq!(
            distinct_roots(&[a.join("in"), a.clone()], &NullProgress).unwrap(),
            std::slice::from_ref(&a)
        );
        let s = scan(
            &[a.join("in"), a.clone(), canon],
            &ExclusionFilter::default(),
            &EmptyFolderSettings::default(),
            &NullProgress,
        )
        .unwrap();
        assert_eq!(s.files.len(), 1, "one file, three ways to reach it: {s:?}");
    }

    /// The old pairwise comparison took ~90 s near the 20,000-root cap and could not be cancelled.
    #[test]
    fn distinct_roots_is_fast_correct_and_cancellable() {
        // Brute force reference (the old definition) on paths that do not exist (kept as given).
        fn reference(roots: &[PathBuf]) -> Vec<PathBuf> {
            roots
                .iter()
                .enumerate()
                .filter(|(i, r)| {
                    !roots
                        .iter()
                        .enumerate()
                        .any(|(j, o)| j != *i && r.starts_with(o) && (*r != o || j < *i))
                })
                .map(|(_, r)| r.clone())
                .collect()
        }
        let base = PathBuf::from("/nonexistent-flect-test-root");
        let p = |s: &str| base.join(s);
        // nested, sibling, prefix-name and repeated cases
        let small: Vec<PathBuf> = [
            "a/b", "a/bc", "a", "a/b/c", "ab", "a/b", "x/y", "x/y/z", "x/yz", "x/y", "x", "q",
        ]
        .iter()
        .map(|s| p(s))
        .collect();
        let got = distinct_roots(&small, &NullProgress).unwrap();
        assert_eq!(got, [p("a"), p("ab"), p("x"), p("q")]);
        assert_eq!(got, reference(&small));
        let siblings: Vec<PathBuf> = ["a/b", "a/bc", "a/b-", "a/b/c", "a/b.d"]
            .iter()
            .map(|s| p(s))
            .collect();
        assert_eq!(
            distinct_roots(&siblings, &NullProgress).unwrap(),
            [p("a/b"), p("a/bc"), p("a/b-"), p("a/b.d")],
            "`a/bc`, `a/b-` and `a/b.d` are not inside `a/b`"
        );
        assert_eq!(
            distinct_roots(&siblings, &NullProgress).unwrap(),
            reference(&siblings)
        );

        // 20,000 roots (the launch cap): mixed nesting, in a scrambled order.
        let mut many = Vec::new();
        for i in 0..20_000u64 {
            let k = (i * 7919) % 20_000;
            many.push(match k % 4 {
                0 => p(&format!("d{}", k / 4)),
                1 => p(&format!("d{}/sub{}", k / 8, k)),
                2 => p(&format!("d{}x/f{}", k / 4, k)),
                _ => p(&format!("e{}", k % 1000)),
            });
        }
        let t = std::time::Instant::now();
        let got = distinct_roots(&many, &NullProgress).unwrap();
        let took = t.elapsed();
        assert!(took < Duration::from_secs(5), "took {took:?}");
        // Check against the reference on a slice small enough for it.
        let slice = &many[..1500];
        assert_eq!(
            distinct_roots(slice, &NullProgress).unwrap(),
            reference(slice)
        );
        // Nothing kept lies inside another kept root, and every dropped root is covered by one.
        let kept: HashSet<&PathBuf> = got.iter().collect();
        for r in &got {
            assert!(
                r.ancestors()
                    .skip(1)
                    .all(|a| !kept.contains(&a.to_path_buf())),
                "{r:?}"
            );
        }
        for r in &many {
            assert!(
                r.ancestors().any(|a| kept.contains(&a.to_path_buf())),
                "{r:?}"
            );
        }

        // Real folders: the resolved spelling still counts as the same folder.
        let d = tempfile::tempdir().unwrap();
        let real: Vec<PathBuf> = (0..3000).map(|i| d.path().join(format!("r{i}"))).collect();
        for r in &real {
            fs::create_dir(r).unwrap();
        }
        let mut with_canon = real.clone();
        with_canon.extend(real.iter().map(|r| fs::canonicalize(r).unwrap()));
        assert_eq!(distinct_roots(&with_canon, &NullProgress).unwrap(), real);

        // Cancelled while resolving.
        struct Stop;
        impl Progress for Stop {
            fn cancelled(&self) -> bool {
                true
            }
        }
        assert_eq!(distinct_roots(&many, &Stop), Err(Cancelled));
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
