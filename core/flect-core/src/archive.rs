//! Archive extraction and creation.
//!
//! Extraction treats every archive as hostile: entry names are sanitized, links and
//! device nodes are never created, sizes are counted as bytes are actually written
//! (headers can lie), and everything lands in a hidden scratch directory that only
//! becomes visible once the whole archive has been processed.
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use crate::paths;
use crate::workspace::{ScratchDirectory, WorkingFile};

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_total_bytes: u64,
    pub max_entries: usize,
    /// Output/input ratio that counts as a bomb once `ratio_floor_bytes` has been written.
    pub max_ratio: u64,
    pub ratio_floor_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_total_bytes: 32 << 30,
            max_entries: 500_000,
            max_ratio: 1000,
            ratio_floor_bytes: 256 << 20,
        }
    }
}

#[derive(Debug)]
pub enum ArchiveError {
    /// Entry name escapes the destination or is otherwise unacceptable.
    UnsafePath(String),
    TooManyEntries,
    TooLarge,
    Bomb,
    Encrypted,
    Unsupported(String),
    Tool(String),
    Io(io::Error),
}

impl ArchiveError {
    pub fn key(&self) -> &'static str {
        match self {
            Self::UnsafePath(_) => "msg.archiveUnsafePath",
            Self::TooManyEntries => "msg.archiveTooManyEntries",
            Self::TooLarge => "msg.archiveTooLarge",
            Self::Bomb => "msg.archiveBomb",
            Self::Encrypted => "msg.archiveEncrypted",
            Self::Unsupported(_) => "msg.archiveUnsupported",
            Self::Tool(_) => "msg.archiveToolFailed",
            Self::Io(e) => crate::paths::io_error_key(e),
        }
    }
}

impl From<io::Error> for ArchiveError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<zip::result::ZipError> for ArchiveError {
    fn from(e: zip::result::ZipError) -> Self {
        match e {
            zip::result::ZipError::Io(e) => Self::Io(e),
            zip::result::ZipError::UnsupportedArchive(m) => Self::Unsupported(m.to_string()),
            other => Self::Unsupported(other.to_string()),
        }
    }
}

/// Turn an entry name into a relative path made only of normal components.
/// Backslashes count as separators (archives from Windows), `..`, absolute
/// paths, drive letters, NUL and empty names are refused.
pub fn sanitize(name: &str) -> Result<PathBuf, ArchiveError> {
    let bad = || ArchiveError::UnsafePath(name.chars().take(200).collect());
    if name.contains('\0') || name.starts_with('/') || name.starts_with('\\') {
        return Err(bad());
    }
    let mut out = PathBuf::new();
    for part in name.split(['/', '\\']) {
        match part {
            "" | "." => continue,
            ".." => return Err(bad()),
            p if p.len() >= 2
                && p.as_bytes()[1] == b':'
                && p.as_bytes()[0].is_ascii_alphabetic() =>
            {
                return Err(bad())
            }
            p => out.push(p),
        }
    }
    if out.as_os_str().is_empty() || out.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(bad());
    }
    Ok(out)
}

#[derive(Default, Debug)]
pub struct Report {
    pub files: usize,
    pub bytes: u64,
    /// (message key, argument) pairs for the UI.
    pub warnings: Vec<(&'static str, String)>,
}

struct Budget {
    limits: Limits,
    compressed: u64,
    entries: usize,
    bytes: u64,
}

impl Budget {
    fn new(limits: Limits, compressed: u64) -> Self {
        Self {
            limits,
            compressed: compressed.max(1),
            entries: 0,
            bytes: 0,
        }
    }
    fn entry(&mut self) -> Result<(), ArchiveError> {
        self.entries += 1;
        if self.entries > self.limits.max_entries {
            return Err(ArchiveError::TooManyEntries);
        }
        Ok(())
    }
    fn add(&mut self, n: u64) -> Result<(), ArchiveError> {
        self.bytes = self.bytes.saturating_add(n);
        if self.bytes > self.limits.max_total_bytes {
            return Err(ArchiveError::TooLarge);
        }
        if self.bytes > self.limits.ratio_floor_bytes
            && self.bytes / self.compressed > self.limits.max_ratio
        {
            return Err(ArchiveError::Bomb);
        }
        Ok(())
    }
}

fn file_mode(mode: Option<u32>) -> u32 {
    // Keep only the owner-exec decision; never carry setuid/setgid/sticky or write-for-others.
    if mode.is_some_and(|m| m & 0o111 != 0) {
        0o755
    } else {
        0o644
    }
}

fn make_parents(root: &Path, rel: &Path) -> io::Result<PathBuf> {
    let target = root.join(rel);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(target)
}

/// Copy at most what the budget allows, charging it as bytes are really written.
fn copy_counted(
    mut src: impl Read,
    dst: &mut File,
    budget: &mut Budget,
) -> Result<u64, ArchiveError> {
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0;
    loop {
        let n = src.read(&mut buf)?;
        if n == 0 {
            return Ok(total);
        }
        budget.add(n as u64)?;
        dst.write_all(&buf[..n])?;
        total += n as u64;
    }
}

fn write_entry(
    root: &Path,
    rel: &Path,
    src: impl Read,
    mode: Option<u32>,
    budget: &mut Budget,
    report: &mut Report,
    display: &str,
) -> Result<(), ArchiveError> {
    let target = make_parents(root, rel)?;
    let mut file = match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(file_mode(mode))
        .open(&target)
    {
        Ok(f) => f,
        // Duplicate name, or two names that collide on a case-insensitive volume.
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            report
                .warnings
                .push(("msg.archiveDuplicateEntry", display.to_owned()));
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };
    let n = copy_counted(src, &mut file, budget)?;
    report.files += 1;
    report.bytes += n;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Zip,
    Tar,
    TarGz,
    SevenZip,
    Rar,
}

pub fn kind_of(path: &Path) -> Option<Kind> {
    let name = path.file_name()?.to_string_lossy().to_lowercase();
    if name.ends_with(".tar.gz") {
        return Some(Kind::TarGz);
    }
    match name.rsplit_once('.')?.1 {
        "zip" => Some(Kind::Zip),
        "tar" => Some(Kind::Tar),
        "7z" => Some(Kind::SevenZip),
        "rar" => Some(Kind::Rar),
        _ => None,
    }
}

/// Destination folder stem: `photos.tar.gz` -> `photos`.
pub fn stem_of(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if name.to_lowercase().ends_with(".tar.gz") {
        return name[..name.len() - 7].to_owned();
    }
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or(name)
}

/// Zips made by Windows tools in a Japanese/Chinese locale store names in CP932 (Shift_JIS)
/// or GBK without setting the UTF-8 flag. A name that is valid UTF-8 (every ASCII name,
/// and anything flagged) is taken as is. For the rest, the first of Shift_JIS and GBK that
/// decodes all of them cleanly is used for the whole archive, so one archive never mixes
/// two guesses. GBK wins when Shift_JIS could only read the names as half-width katakana,
/// which is what Chinese text looks like through the wrong decoder. Nothing fits: None,
/// and the zip crate's own CP437 reading stays.
fn legacy_name_encoding(raw_names: &[&[u8]]) -> Option<&'static encoding_rs::Encoding> {
    use encoding_rs::{GBK, SHIFT_JIS};
    let legacy: Vec<&[u8]> = raw_names
        .iter()
        .copied()
        .filter(|n| std::str::from_utf8(n).is_err())
        .collect();
    if legacy.is_empty() {
        return None;
    }
    let decodes = |enc: &'static encoding_rs::Encoding| -> Option<Vec<String>> {
        legacy
            .iter()
            .map(|n| {
                enc.decode_without_bom_handling_and_without_replacement(n)
                    .map(|c| c.into_owned())
            })
            .collect()
    };
    let half_width_kana = |c: char| ('\u{FF61}'..='\u{FF9F}').contains(&c);
    match (decodes(SHIFT_JIS), decodes(GBK)) {
        (Some(sjis), gbk) => {
            let kana_only = sjis.iter().any(|n| n.chars().any(half_width_kana));
            if kana_only && gbk.is_some() {
                Some(GBK)
            } else {
                Some(SHIFT_JIS)
            }
        }
        (None, Some(_)) => Some(GBK),
        (None, None) => None,
    }
}

/// Entry name for extraction: UTF-8 if it is, else decoded with the archive's legacy
/// encoding; `fallback` (the zip crate's reading) when no encoding fits.
fn zip_entry_name(
    raw: &[u8],
    legacy: Option<&'static encoding_rs::Encoding>,
    fallback: impl FnOnce() -> Result<String, ArchiveError>,
) -> Result<String, ArchiveError> {
    if let Ok(utf8) = std::str::from_utf8(raw) {
        return Ok(utf8.to_owned());
    }
    legacy
        .and_then(|enc| enc.decode_without_bom_handling_and_without_replacement(raw))
        .map(|c| c.into_owned())
        .map_or_else(fallback, Ok)
}

pub fn extract_zip(archive: &Path, into: &Path, limits: Limits) -> Result<Report, ArchiveError> {
    let file = File::open(archive)?;
    let size = file.metadata()?.len();
    let mut zip = zip::ZipArchive::new(file)?;
    let legacy = {
        let mut raw_names: Vec<Vec<u8>> = Vec::with_capacity(zip.len().min(4096));
        for i in 0..zip.len() {
            // Headers only: nothing is decompressed or decrypted here.
            let raw = zip.by_index_raw(i)?.name_raw().to_vec();
            if std::str::from_utf8(&raw).is_err() {
                raw_names.push(raw);
            }
        }
        legacy_name_encoding(&raw_names.iter().map(Vec::as_slice).collect::<Vec<_>>())
    };
    let mut budget = Budget::new(limits, size);
    let mut report = Report::default();
    for i in 0..zip.len() {
        budget.entry()?;
        let mut entry = zip.by_index(i)?;
        let name = zip_entry_name(entry.name_raw(), legacy, || Ok(entry.name()?.into_owned()))?;
        if entry.encrypted() {
            return Err(ArchiveError::Encrypted);
        }
        let rel = sanitize(&name)?;
        // Decided from the decoded name: the zip crate looks at the raw last byte, and a legacy
        // (Shift_JIS ...) multi-byte character can end in 0x5C / 0x2F without being a separator.
        if name.ends_with('/') || name.ends_with('\\') {
            fs::create_dir_all(into.join(&rel))?;
            continue;
        }
        let mode = entry.unix_mode();
        if entry.is_symlink() || mode.is_some_and(|m| m & 0o170000 != 0o100000 && m & 0o170000 != 0)
        {
            report.warnings.push(("msg.archiveSkippedLink", name));
            continue;
        }
        write_entry(
            into,
            &rel,
            &mut entry,
            mode,
            &mut budget,
            &mut report,
            &name,
        )?;
    }
    Ok(report)
}

pub fn extract_tar(
    reader: impl Read,
    compressed_size: u64,
    into: &Path,
    limits: Limits,
) -> Result<Report, ArchiveError> {
    let mut tar = tar::Archive::new(reader);
    tar.set_preserve_permissions(false);
    tar.set_unpack_xattrs(false);
    let mut budget = Budget::new(limits, compressed_size);
    let mut report = Report::default();
    for entry in tar.entries()? {
        budget.entry()?;
        let mut entry = entry?;
        let name = entry.path()?.to_string_lossy().into_owned();
        let kind = entry.header().entry_type();
        let mode = entry.header().mode().ok();
        match kind {
            tar::EntryType::Directory => fs::create_dir_all(into.join(sanitize(&name)?))?,
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                let rel = sanitize(&name)?;
                write_entry(
                    into,
                    &rel,
                    &mut entry,
                    mode,
                    &mut budget,
                    &mut report,
                    &name,
                )?;
            }
            // Headers carrying metadata for the next entry.
            tar::EntryType::XGlobalHeader
            | tar::EntryType::XHeader
            | tar::EntryType::GNULongName
            | tar::EntryType::GNULongLink => {}
            _ => report.warnings.push(("msg.archiveSkippedLink", name)),
        }
    }
    Ok(report)
}

pub fn extract_tar_gz(archive: &Path, into: &Path, limits: Limits) -> Result<Report, ArchiveError> {
    let file = File::open(archive)?;
    let size = file.metadata()?.len();
    extract_tar(flate2::read::GzDecoder::new(file), size, into, limits)
}

// ---------------------------------------------------------------- external 7-Zip

/// Fixed install locations only: a GUI-launched app has a thin PATH, and trusting
/// PATH would let any directory on it supply the binary.
const TOOL_DIRS: [&str; 2] = ["/opt/homebrew/bin", "/usr/local/bin"];

pub fn find_tool(name: &str) -> Option<PathBuf> {
    TOOL_DIRS
        .iter()
        .map(|d| Path::new(d).join(name))
        .find(|p| tool_is_trustworthy(p))
}

/// A regular, executable file that neither it nor its directory lets "others" modify.
pub fn tool_is_trustworthy(path: &Path) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    if !meta.is_file()
        || meta.permissions().mode() & 0o111 == 0
        || meta.permissions().mode() & 0o002 != 0
    {
        return false;
    }
    path.parent()
        .and_then(|d| fs::metadata(d).ok())
        .is_some_and(|m| m.permissions().mode() & 0o002 == 0)
}

#[derive(Debug, PartialEq, Eq)]
pub struct Listed {
    pub entries: usize,
    pub total_bytes: u64,
}

/// Parse `7zz l -slt` output.
pub fn parse_listing(output: &str) -> Listed {
    let (mut entries, mut total) = (0usize, 0u64);
    let mut in_entries = false;
    let (mut has_path, mut size, mut folder) = (false, 0u64, false);
    let mut flush = |has_path: &mut bool, size: &mut u64, folder: &mut bool| {
        if *has_path {
            entries += 1;
            if !*folder {
                total = total.saturating_add(*size);
            }
        }
        *has_path = false;
        *size = 0;
        *folder = false;
    };
    for line in output.lines() {
        if line.starts_with("----------") {
            in_entries = true;
            continue;
        }
        if !in_entries {
            continue;
        }
        if line.trim().is_empty() {
            flush(&mut has_path, &mut size, &mut folder);
        } else if let Some(v) = line.strip_prefix("Size = ") {
            size = v.trim().parse().unwrap_or(0);
        } else if line.starts_with("Path = ") {
            has_path = true;
        } else if let Some(v) = line.strip_prefix("Folder = ") {
            folder = v.trim() == "+";
        }
    }
    flush(&mut has_path, &mut size, &mut folder);
    Listed {
        entries,
        total_bytes: total,
    }
}

fn run_tool(tool: &Path, args: &[&std::ffi::OsStr]) -> Result<String, ArchiveError> {
    let out = std::process::Command::new(tool)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| ArchiveError::Tool(e.to_string()))?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    match out.status.code() {
        Some(0) | Some(1) => Ok(text), // 1 = warnings only
        code => {
            let err = String::from_utf8_lossy(&out.stderr);
            let tail: Vec<&str> = err.lines().rev().take(3).collect();
            Err(ArchiveError::Tool(format!(
                "exit {code:?}: {}",
                tail.into_iter().rev().collect::<Vec<_>>().join(" ")
            )))
        }
    }
}

/// Check what an external extractor produced: only regular files and directories
/// may remain, and nothing may resolve outside `root`.
pub fn audit_tree(root: &Path, report: &mut Report) -> Result<(), ArchiveError> {
    let canonical_root = fs::canonicalize(root)?;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let meta = fs::symlink_metadata(&path)?;
            let ft = meta.file_type();
            if ft.is_dir() {
                if !fs::canonicalize(&path)?.starts_with(&canonical_root) {
                    return Err(ArchiveError::UnsafePath(
                        path.to_string_lossy().into_owned(),
                    ));
                }
                stack.push(path);
            } else if ft.is_file() {
                report.files += 1;
                report.bytes += meta.len();
                let m = meta.permissions().mode();
                if m & 0o7000 != 0 || m & 0o002 != 0 {
                    fs::set_permissions(&path, fs::Permissions::from_mode(file_mode(Some(m))))?;
                }
            } else {
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned();
                fs::remove_file(&path)?;
                report.warnings.push(("msg.archiveSkippedLink", rel));
            }
        }
    }
    Ok(())
}

pub fn extract_with_7zip(
    tool: &Path,
    archive: &Path,
    into: &Path,
    limits: Limits,
) -> Result<Report, ArchiveError> {
    use std::ffi::OsStr;
    // Absolute archive path, so it can never be mistaken for a switch.
    let archive = fs::canonicalize(archive)?;
    let listing = parse_listing(&run_tool(
        tool,
        &[
            OsStr::new("l"),
            OsStr::new("-slt"),
            OsStr::new("-ba"),
            OsStr::new("-bd"),
            OsStr::new("--"),
            archive.as_os_str(),
        ],
    )?);
    // `-ba` drops the header; re-run without it if the parser saw nothing but the archive is not empty.
    let listing = if listing.entries == 0 {
        parse_listing(&run_tool(
            tool,
            &[
                OsStr::new("l"),
                OsStr::new("-slt"),
                OsStr::new("-bd"),
                OsStr::new("--"),
                archive.as_os_str(),
            ],
        )?)
    } else {
        listing
    };
    if listing.entries > limits.max_entries {
        return Err(ArchiveError::TooManyEntries);
    }
    if listing.total_bytes > limits.max_total_bytes {
        return Err(ArchiveError::TooLarge);
    }
    let size = fs::metadata(&archive)?.len().max(1);
    if listing.total_bytes > limits.ratio_floor_bytes
        && listing.total_bytes / size > limits.max_ratio
    {
        return Err(ArchiveError::Bomb);
    }
    let mut out_arg = std::ffi::OsString::from("-o");
    out_arg.push(into);
    run_tool(
        tool,
        &[
            OsStr::new("x"),
            OsStr::new("-y"),
            OsStr::new("-bd"),
            OsStr::new("-snl-"),
            &out_arg,
            OsStr::new("--"),
            archive.as_os_str(),
        ],
    )?;
    let mut report = Report::default();
    audit_tree(into, &mut report)?;
    Ok(report)
}

// ---------------------------------------------------------------- the extract task

#[derive(Debug)]
pub struct Extracted {
    pub destination: PathBuf,
    pub report: Report,
}

/// Extract `archive` into a new folder beside it (`name`, `name (2)`, ...).
pub fn extract(
    archive: &Path,
    limits: Limits,
    seven_zip: Option<&Path>,
) -> Result<Extracted, ArchiveError> {
    extract_to(archive, limits, seven_zip, "")
}

/// [`extract`] into the output folder `folder` names (see `paths::output_dir`); empty = beside.
pub fn extract_to(
    archive: &Path,
    limits: Limits,
    seven_zip: Option<&Path>,
    folder: &str,
) -> Result<Extracted, ArchiveError> {
    let kind = kind_of(archive)
        .ok_or_else(|| ArchiveError::Unsupported(archive.to_string_lossy().into_owned()))?;
    let out_dir = paths::prepare_output_dir(archive, folder)?;
    // Staged inside the destination folder so the final rename stays on one volume.
    let scratch = ScratchDirectory::new(&out_dir.join(stem_of(archive)))?;
    let staging = scratch.path_for("out");
    fs::create_dir(&staging)?;
    let report = match kind {
        Kind::Zip => extract_zip(archive, &staging, limits)?,
        Kind::Tar => extract_tar(
            File::open(archive)?,
            fs::metadata(archive)?.len(),
            &staging,
            limits,
        )?,
        Kind::TarGz => extract_tar_gz(archive, &staging, limits)?,
        Kind::SevenZip | Kind::Rar => {
            let tool = seven_zip.ok_or_else(|| {
                ArchiveError::Tool("7zz not found (brew install sevenzip)".into())
            })?;
            extract_with_7zip(tool, archive, &staging, limits)?
        }
    };
    // Claim a free visible name, then move the finished tree onto it.
    for _ in 0..16 {
        let dest = paths::derive_directory_in(&out_dir, archive, Some(&stem_of(archive)), "");
        match fs::create_dir(&dest) {
            Ok(()) => {
                fs::rename(&staging, &dest)?;
                return Ok(Extracted {
                    destination: dest,
                    report,
                });
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(ArchiveError::Io(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free folder name",
    )))
}

// ---------------------------------------------------------------- creation

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Zip,
    Tar,
    TarGz,
    SevenZip,
}

impl Format {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Zip => ".zip",
            Self::Tar => ".tar",
            Self::TarGz => ".tar.gz",
            Self::SevenZip => ".7z",
        }
    }
}

fn collect(root: &Path, report: &mut Report) -> io::Result<Vec<(PathBuf, bool)>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut names: Vec<_> = fs::read_dir(&dir)?.flatten().collect();
        names.sort_by_key(|e| e.file_name());
        for e in names {
            let path = e.path();
            let ft = e.file_type()?;
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with(&format!(".{}-", crate::workspace::BRAND)) {
                continue; // our own scratch files
            }
            if ft.is_dir() {
                out.push((path.clone(), true));
                stack.push(path);
            } else if ft.is_file() {
                out.push((path, false));
            } else {
                report.warnings.push(("msg.archiveSkippedLink", name));
            }
        }
    }
    out.sort();
    Ok(out)
}

fn rel_name(base: &Path, path: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Pack `folder` (the folder itself is the single top-level entry) into `out`.
pub fn create(
    folder: &Path,
    format: Format,
    out: &Path,
    seven_zip: Option<&Path>,
) -> Result<Report, ArchiveError> {
    let parent = folder.parent().unwrap_or(Path::new("."));
    let mut report = Report::default();
    let items = collect(folder, &mut report)?;
    let top = rel_name(parent, folder);
    match format {
        Format::Zip => {
            let mut w = zip::ZipWriter::new(File::create(out)?);
            let base = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .large_file(true);
            w.add_directory(format!("{top}/"), base)?;
            for (path, is_dir) in &items {
                let name = rel_name(parent, path);
                let meta = fs::metadata(path)?;
                let opts = base.unix_permissions(meta.permissions().mode() & 0o777);
                if *is_dir {
                    w.add_directory(format!("{name}/"), opts)?;
                } else {
                    w.start_file(name, opts)?;
                    io::copy(&mut File::open(path)?, &mut w)?;
                    report.files += 1;
                }
            }
            w.finish()?;
        }
        Format::Tar | Format::TarGz => {
            let file = File::create(out)?;
            let sink: Box<dyn Write> = if format == Format::TarGz {
                Box::new(flate2::write::GzEncoder::new(
                    file,
                    flate2::Compression::default(),
                ))
            } else {
                Box::new(file)
            };
            let mut b = tar::Builder::new(sink);
            b.follow_symlinks(false);
            b.append_dir(&top, folder)?;
            for (path, is_dir) in &items {
                let name = rel_name(parent, path);
                if *is_dir {
                    b.append_dir(&name, path)?;
                } else {
                    b.append_path_with_name(path, &name)?;
                    report.files += 1;
                }
            }
            b.into_inner()?.flush()?;
        }
        Format::SevenZip => {
            let tool = seven_zip.ok_or_else(|| {
                ArchiveError::Tool("7zz not found (brew install sevenzip)".into())
            })?;
            let out_abs = out.to_path_buf();
            let status = std::process::Command::new(tool)
                .args(["a", "-t7z", "-bd", "-snl-"])
                .arg(&out_abs)
                .arg("--")
                .arg(folder.file_name().unwrap_or_default())
                .current_dir(parent)
                .stdin(std::process::Stdio::null())
                .output()
                .map_err(|e| ArchiveError::Tool(e.to_string()))?;
            if !matches!(status.status.code(), Some(0) | Some(1)) {
                return Err(ArchiveError::Tool(
                    String::from_utf8_lossy(&status.stderr)
                        .lines()
                        .last()
                        .unwrap_or("")
                        .to_owned(),
                ));
            }
        }
    }
    Ok(report)
}

/// Compress a folder beside itself, never overwriting an existing archive.
pub fn compress_folder(
    folder: &Path,
    format: Format,
    seven_zip: Option<&Path>,
) -> Result<(PathBuf, Report), ArchiveError> {
    compress_folder_to(folder, format, seven_zip, "")
}

/// [`compress_folder`] into the output folder `into` names (see `paths::output_dir`;
/// relative paths start from the folder's parent); empty = beside the folder.
pub fn compress_folder_to(
    folder: &Path,
    format: Format,
    seven_zip: Option<&Path>,
    into: &str,
) -> Result<(PathBuf, Report), ArchiveError> {
    if !fs::symlink_metadata(folder)?.is_dir() {
        return Err(ArchiveError::Unsupported("not a folder".into()));
    }
    let out_dir = paths::output_dir(folder, into);
    // An archive written inside the folder it packs would swallow itself.
    if out_dir.starts_with(folder) {
        return Err(ArchiveError::Unsupported(
            "output folder is inside the folder being compressed".into(),
        ));
    }
    paths::prepare_output_dir(folder, into)?;
    let scratch = ScratchDirectory::new(folder)?;
    let ext = format.extension();
    let staged = scratch.path_for(&format!("out{ext}"));
    let mut guard = WorkingFile::new(staged.clone());
    let report = create(folder, format, &staged, seven_zip)?;
    for _ in 0..16 {
        let target = paths::derive_in(&out_dir, folder, "", Some(ext));
        match crate::workspace::commit_new(&staged, &target) {
            Ok(()) => {
                guard.keep();
                return Ok((target, report));
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            // Different volume for the scratch dir is impossible (it is a sibling), but be explicit.
            Err(e) => return Err(e.into()),
        }
    }
    Err(ArchiveError::Io(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free archive name",
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let o = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in entries {
            w.start_file(*name, o).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    /// A zip whose entry names are raw bytes with no UTF-8 flag, like Windows tools write.
    /// Each name is written as an ASCII placeholder of the same length, then patched in
    /// (the CRC covers data only, so the archive stays valid).
    fn zip_with_raw_names(entries: &[(&[u8], &[u8])]) -> Vec<u8> {
        let placeholders: Vec<String> = entries
            .iter()
            .enumerate()
            .map(|(i, (name, _))| format!("{:0>width$}", format!("~{i}~"), width = name.len()))
            .collect();
        let named: Vec<(&str, &[u8])> = placeholders
            .iter()
            .zip(entries)
            .map(|(p, (_, data))| (p.as_str(), *data))
            .collect();
        let mut bytes = zip_bytes(&named);
        for (p, (raw, _)) in placeholders.iter().zip(entries) {
            let at: Vec<usize> = bytes
                .windows(p.len())
                .enumerate()
                .filter(|(_, w)| *w == p.as_bytes())
                .map(|(i, _)| i)
                .collect();
            assert_eq!(at.len(), 2, "local + central header");
            for i in at {
                bytes[i..i + raw.len()].copy_from_slice(raw);
            }
        }
        bytes
    }

    fn encoded(enc: &'static encoding_rs::Encoding, text: &str) -> Vec<u8> {
        let (bytes, _, lossy) = enc.encode(text);
        assert!(!lossy, "{text} is not representable");
        bytes.into_owned()
    }

    fn extract_names(zip: Vec<u8>) -> Result<Vec<String>, ArchiveError> {
        let d = tempfile::tempdir().unwrap();
        let zp = d.path().join("a.zip");
        let out = d.path().join("out");
        fs::create_dir(&out).unwrap();
        fs::write(&zp, zip).unwrap();
        extract_zip(&zp, &out, Limits::default())?;
        let mut names = Vec::new();
        let mut stack = vec![out.clone()];
        while let Some(dir) = stack.pop() {
            for e in fs::read_dir(dir).unwrap().flatten() {
                if e.file_type().unwrap().is_dir() {
                    stack.push(e.path());
                } else {
                    names.push(
                        e.path()
                            .strip_prefix(&out)
                            .unwrap()
                            .to_string_lossy()
                            .into_owned(),
                    );
                }
            }
        }
        names.sort();
        Ok(names)
    }

    #[test]
    fn zip_shift_jis_names_are_decoded() {
        // 表 ends in 0x5C (a backslash byte): it must not be read as a path separator.
        let dir = encoded(encoding_rs::SHIFT_JIS, "報告書/表.txt");
        assert!(dir.contains(&0x5C));
        let zip = zip_with_raw_names(&[
            (&dir, b"1"),
            (&encoded(encoding_rs::SHIFT_JIS, "日本語.txt"), b"2"),
            (b"plain.txt", b"3"),
        ]);
        assert_eq!(
            extract_names(zip).unwrap(),
            ["plain.txt", "報告書/表.txt", "日本語.txt"]
        );
    }

    #[test]
    fn zip_legacy_name_ending_in_a_backslash_byte_is_a_file_not_a_directory() {
        // The whole name 報告書/表 ends with the bytes 95 5C: a file called 表, not a folder.
        let name = encoded(encoding_rs::SHIFT_JIS, "報告書/表");
        assert_eq!(name.last(), Some(&0x5C));
        let zip = zip_with_raw_names(&[(&name, b"data"), (b"real_dir/", b"")]);
        assert_eq!(extract_names(zip).unwrap(), ["報告書/表"]);
    }

    #[test]
    fn zip_gbk_names_are_decoded() {
        let zip = zip_with_raw_names(&[
            (&encoded(encoding_rs::GBK, "中文/说明.txt"), b"1"),
            (&encoded(encoding_rs::GBK, "资料.txt"), b"2"),
        ]);
        assert_eq!(extract_names(zip).unwrap(), ["中文/说明.txt", "资料.txt"]);
    }

    #[test]
    fn zip_utf8_names_are_untouched_next_to_legacy_ones() {
        let zip = zip_with_raw_names(&[
            ("日本語.txt".as_bytes(), b"1"),
            (&encoded(encoding_rs::SHIFT_JIS, "資料.txt"), b"2"),
        ]);
        assert_eq!(extract_names(zip).unwrap(), ["日本語.txt", "資料.txt"]);
    }

    #[test]
    fn zip_names_no_encoding_fits_fall_back_without_failing() {
        // 0xFF is not a lead byte in Shift_JIS or GBK: the zip crate's CP437 reading is kept.
        let zip = zip_with_raw_names(&[(b"a\xFF\xFFb.txt", b"1")]);
        let names = extract_names(zip).unwrap();
        assert_eq!(names.len(), 1);
        assert!(names[0].starts_with('a') && names[0].ends_with("b.txt"));
    }

    #[test]
    fn zip_legacy_names_keep_the_safety_checks() {
        for bad in ["../報告.txt", "..\\報告.txt", "/報告.txt", "C:\\報告.txt"] {
            let zip = zip_with_raw_names(&[(&encoded(encoding_rs::SHIFT_JIS, bad), b"x")]);
            let err = extract_names(zip).unwrap_err();
            assert_eq!(err.key(), "msg.archiveUnsafePath", "{bad}");
        }
    }

    #[test]
    fn legacy_encoding_choice() {
        use encoding_rs::{GBK, SHIFT_JIS};
        let sjis = encoded(SHIFT_JIS, "日本語のファイル");
        let gbk = encoded(GBK, "中文文件说明");
        assert_eq!(legacy_name_encoding(&[b"ascii", "ü".as_bytes()]), None);
        assert_eq!(legacy_name_encoding(&[&sjis]), Some(SHIFT_JIS));
        assert_eq!(legacy_name_encoding(&[&gbk]), Some(GBK));
        assert_eq!(legacy_name_encoding(&[b"\xFF\xFF"]), None);
    }

    fn tar_with(entries: &[(&str, tar::EntryType, &[u8], &str)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, kind, data, link) in entries {
            let mut h = tar::Header::new_gnu();
            h.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes()); // bypass the builder's own ".." check
            h.set_entry_type(*kind);
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            if !link.is_empty() {
                h.set_link_name(link).unwrap();
            }
            h.set_cksum();
            out.extend_from_slice(h.as_bytes());
            out.extend_from_slice(data);
            out.resize(out.len().div_ceil(512) * 512, 0);
        }
        out.extend_from_slice(&[0u8; 1024]);
        out
    }

    fn snapshot(dir: &Path) -> Vec<String> {
        let mut v = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(&d).unwrap().flatten() {
                v.push(
                    e.path()
                        .strip_prefix(dir)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
                if e.file_type().unwrap().is_dir() {
                    stack.push(e.path());
                }
            }
        }
        v.sort();
        v
    }

    #[test]
    fn sanitize_accepts_normal_and_refuses_hostile() {
        assert_eq!(sanitize("a/b/c.txt").unwrap(), PathBuf::from("a/b/c.txt"));
        assert_eq!(sanitize("./a//b").unwrap(), PathBuf::from("a/b"));
        assert_eq!(
            sanitize("日本語/ファイル 1.txt").unwrap(),
            PathBuf::from("日本語/ファイル 1.txt")
        );
        assert_eq!(sanitize("a\\b.txt").unwrap(), PathBuf::from("a/b.txt")); // Windows separators
        for bad in [
            "../x",
            "a/../../x",
            "/etc/passwd",
            "\\evil",
            "C:\\x",
            "c:/x",
            "a/..\\..\\x",
            "",
            ".",
            "a\0b",
            "..",
        ] {
            assert!(sanitize(bad).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn zip_traversal_is_refused_and_writes_nothing_outside() {
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("out");
        fs::create_dir(&out).unwrap();
        for name in [
            "../escape.txt",
            "/abs.txt",
            "ok/../../escape.txt",
            "..\\escape.txt",
            "C:\\win.txt",
        ] {
            let zp = d.path().join("evil.zip");
            fs::write(&zp, zip_bytes(&[("fine.txt", b"1"), (name, b"x")])).unwrap();
            let err = extract_zip(&zp, &out, Limits::default()).unwrap_err();
            assert_eq!(err.key(), "msg.archiveUnsafePath", "{name}");
        }
        assert!(!d.path().join("escape.txt").exists());
        assert!(!Path::new("/abs.txt").exists());
    }

    #[test]
    fn zip_symlink_entries_are_not_created() {
        let d = tempfile::tempdir().unwrap();
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let o = zip::write::SimpleFileOptions::default();
        w.add_symlink("link", "/etc/passwd", o).unwrap();
        w.start_file("real.txt", o).unwrap();
        w.write_all(b"hi").unwrap();
        let zp = d.path().join("l.zip");
        fs::write(&zp, w.finish().unwrap().into_inner()).unwrap();
        let out = d.path().join("out");
        fs::create_dir(&out).unwrap();
        let r = extract_zip(&zp, &out, Limits::default()).unwrap();
        assert_eq!(snapshot(&out), ["real.txt"]);
        assert_eq!(r.warnings, [("msg.archiveSkippedLink", "link".to_string())]);
    }

    #[test]
    fn zip_bomb_and_size_limits_trip_on_real_output() {
        let d = tempfile::tempdir().unwrap();
        let zeros = vec![0u8; 20 << 20];
        let zp = d.path().join("bomb.zip");
        fs::write(&zp, zip_bytes(&[("zeros", &zeros)])).unwrap();
        let out = d.path().join("out");
        fs::create_dir(&out).unwrap();
        let tight = Limits {
            max_ratio: 50,
            ratio_floor_bytes: 1 << 20,
            ..Limits::default()
        };
        assert_eq!(
            extract_zip(&zp, &out, tight).unwrap_err().key(),
            "msg.archiveBomb"
        );
        let small = Limits {
            max_total_bytes: 1 << 20,
            ..Limits::default()
        };
        assert_eq!(
            extract_zip(&zp, &d.path().join("o2"), small)
                .unwrap_err()
                .key(),
            "msg.archiveTooLarge"
        );
        let few = Limits {
            max_entries: 1,
            ..Limits::default()
        };
        let many = d.path().join("many.zip");
        fs::write(&many, zip_bytes(&[("a", b"1"), ("b", b"2")])).unwrap();
        assert_eq!(
            extract_zip(&many, &d.path().join("o3"), few)
                .unwrap_err()
                .key(),
            "msg.archiveTooManyEntries"
        );
        // the default limits accept a legitimate, highly compressible file
        fs::create_dir(d.path().join("o4")).unwrap();
        assert!(extract_zip(&zp, &d.path().join("o4"), Limits::default()).is_ok());
    }

    #[test]
    fn duplicate_entries_warn_and_keep_the_first() {
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("out");
        fs::create_dir(&out).unwrap();
        // The zip writer refuses duplicate names, so build the hostile archive as a tar.
        let t = tar_with(&[
            ("a.txt", tar::EntryType::Regular, b"first", ""),
            ("a.txt", tar::EntryType::Regular, b"second", ""),
        ]);
        let r = extract_tar(Cursor::new(t), 100, &out, Limits::default()).unwrap();
        assert_eq!(fs::read(out.join("a.txt")).unwrap(), b"first");
        assert_eq!(r.warnings[0].0, "msg.archiveDuplicateEntry");
    }

    #[test]
    fn modes_are_normalized() {
        let d = tempfile::tempdir().unwrap();
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (n, m) in [("setuid", 0o4777u32), ("exec", 0o755), ("plain", 0o600)] {
            w.start_file(
                n,
                zip::write::SimpleFileOptions::default().unix_permissions(m),
            )
            .unwrap();
        }
        let zp = d.path().join("m.zip");
        fs::write(&zp, w.finish().unwrap().into_inner()).unwrap();
        let out = d.path().join("out");
        fs::create_dir(&out).unwrap();
        extract_zip(&zp, &out, Limits::default()).unwrap();
        let mode = |n: &str| fs::metadata(out.join(n)).unwrap().permissions().mode() & 0o7777;
        assert_eq!(
            (mode("setuid"), mode("exec"), mode("plain")),
            (0o755, 0o755, 0o644)
        );
    }

    #[test]
    fn tar_hostile_entries() {
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("out");
        fs::create_dir(&out).unwrap();
        let t = tar_with(&[("../escape", tar::EntryType::Regular, b"x", "")]);
        assert_eq!(
            extract_tar(Cursor::new(t), 100, &out, Limits::default())
                .unwrap_err()
                .key(),
            "msg.archiveUnsafePath"
        );
        assert!(!d.path().join("escape").exists());

        let t = tar_with(&[
            ("ok.txt", tar::EntryType::Regular, b"fine", ""),
            ("sym", tar::EntryType::Symlink, b"", "/etc/passwd"),
            ("hard", tar::EntryType::Link, b"", "ok.txt"),
            ("dev", tar::EntryType::Char, b"", ""),
            ("sub/", tar::EntryType::Directory, b"", ""),
        ]);
        let r = extract_tar(Cursor::new(t), 100, &out, Limits::default()).unwrap();
        assert_eq!(snapshot(&out), ["ok.txt", "sub"]);
        assert_eq!(r.warnings.len(), 3);
    }

    #[test]
    fn create_and_extract_round_trip_all_native_formats() {
        for (format, name) in [
            (Format::Zip, "zip"),
            (Format::Tar, "tar"),
            (Format::TarGz, "tar.gz"),
        ] {
            let d = tempfile::tempdir().unwrap();
            let src = d.path().join("資料 1");
            fs::create_dir_all(src.join("sub/deep")).unwrap();
            fs::write(src.join("a.txt"), "あ").unwrap();
            fs::write(src.join("sub/deep/b.bin"), vec![9u8; 100_000]).unwrap();
            fs::create_dir(src.join("emptydir")).unwrap();
            std::os::unix::fs::symlink("/etc/passwd", src.join("link")).unwrap();
            let (archive, rep) = compress_folder(&src, format, None).unwrap();
            assert_eq!(archive, d.path().join(format!("資料 1.{name}")));
            assert_eq!(
                rep.warnings.len(),
                1,
                "{name}: symlink skipped with a warning"
            );
            assert_eq!(rep.files, 2);
            let x = extract(&archive, Limits::default(), None).unwrap();
            assert_eq!(x.destination, d.path().join("資料 1 (2)"), "{name}"); // the source folder already holds the name
            let expected = [
                "資料 1",
                "資料 1/a.txt",
                "資料 1/emptydir",
                "資料 1/sub",
                "資料 1/sub/deep",
                "資料 1/sub/deep/b.bin",
            ];
            assert_eq!(snapshot(&x.destination), expected);
            assert_eq!(
                fs::read_to_string(x.destination.join("資料 1/a.txt")).unwrap(),
                "あ"
            );
            // second compress never overwrites
            let (again, _) = compress_folder(&src, format, None).unwrap();
            assert_eq!(again, d.path().join(format!("資料 1 (2).{name}")));
            // no scratch litter
            assert!(fs::read_dir(d.path())
                .unwrap()
                .flatten()
                .all(|e| !e.file_name().to_string_lossy().starts_with(".Flect")));
        }
    }

    #[test]
    fn failed_extraction_leaves_nothing_behind() {
        let d = tempfile::tempdir().unwrap();
        let zp = d.path().join("evil.zip");
        fs::write(&zp, zip_bytes(&[("ok.txt", b"1"), ("../x", b"2")])).unwrap();
        assert!(extract(&zp, Limits::default(), None).is_err());
        assert_eq!(snapshot(d.path()), ["evil.zip"]);
    }

    #[test]
    fn kinds_and_stems() {
        assert_eq!(kind_of(Path::new("/a/B.TAR.GZ")), Some(Kind::TarGz));
        assert_eq!(kind_of(Path::new("x.rar")), Some(Kind::Rar));
        assert_eq!(kind_of(Path::new("x.txt")), None);
        assert_eq!(stem_of(Path::new("/a/photos.tar.gz")), "photos");
        assert_eq!(stem_of(Path::new("/a/data.v2.zip")), "data.v2");
    }

    const LISTING: &str = "\nListing archive: /x.7z\n\n--\nPath = /x.7z\nType = 7z\n\n----------\nPath = d\nFolder = +\nSize = 0\n\nPath = d/a.txt\nFolder = -\nSize = 1500\n\nPath = b.bin\nSize = 2500\nFolder = -\n\n";

    #[test]
    fn parses_7zip_listing() {
        assert_eq!(
            parse_listing(LISTING),
            Listed {
                entries: 3,
                total_bytes: 4000
            }
        );
        assert_eq!(
            parse_listing("garbage"),
            Listed {
                entries: 0,
                total_bytes: 0
            }
        );
    }

    fn fake_7zz(dir: &Path, listing_size: u64) -> PathBuf {
        let mark = dir.join("extracted").display().to_string();
        let script = dir.join("7zz");
        let body = format!(
            r#"#!/bin/sh
case "$1" in
  l) printf '%s\n' '----------' 'Path = a.txt' 'Folder = -' 'Size = {listing_size}' '' ;;
  x) for a in "$@"; do case "$a" in -o*) out="${{a#-o}}";; esac; done
     echo hello > "$out/a.txt"; mkdir -p "$out/d"; echo x > "$out/d/b.txt"
     ln -s /etc/passwd "$out/evil"; chmod 4755 "$out/a.txt"; touch "{mark}" ;;
esac
exit 0
"#
        );
        fs::write(&script, body).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    #[test]
    fn seven_zip_flow_audits_the_output() {
        let d = tempfile::tempdir().unwrap();
        let tool = fake_7zz(d.path(), 10);
        assert!(tool_is_trustworthy(&tool));
        let arc = d.path().join("pack.7z");
        fs::write(&arc, b"not really").unwrap();
        let x = extract(&arc, Limits::default(), Some(&tool)).unwrap();
        assert_eq!(x.destination, d.path().join("pack"));
        assert_eq!(snapshot(&x.destination), ["a.txt", "d", "d/b.txt"]); // symlink removed
        assert_eq!(x.report.warnings.len(), 1);
        assert_eq!(
            fs::metadata(x.destination.join("a.txt"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o755
        ); // setuid stripped
    }

    #[test]
    fn seven_zip_listing_limits_apply_before_extracting() {
        let d = tempfile::tempdir().unwrap();
        let tool = fake_7zz(d.path(), u64::MAX / 2);
        let arc = d.path().join("big.7z");
        fs::write(&arc, b"x").unwrap();
        let mark = d.path().join("extracted");
        let err = extract(&arc, Limits::default(), Some(&tool)).unwrap_err();
        assert_eq!(err.key(), "msg.archiveTooLarge");
        assert!(
            !mark.exists(),
            "7zz x must not run when the listing already exceeds the limits"
        );
    }

    #[test]
    fn missing_or_untrusted_tools_are_not_used() {
        let d = tempfile::tempdir().unwrap();
        assert!(!tool_is_trustworthy(&d.path().join("nope")));
        let t = fake_7zz(d.path(), 1);
        fs::set_permissions(&t, fs::Permissions::from_mode(0o757)).unwrap(); // world-writable
        assert!(!tool_is_trustworthy(&t));
        assert_eq!(
            extract(&d.path().join("a.7z"), Limits::default(), None)
                .unwrap_err()
                .key(),
            "msg.archiveToolFailed"
        );
    }
}
