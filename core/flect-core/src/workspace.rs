//! Scratch files and directories that clean up after themselves, plus a
//! no-overwrite commit so a finished result never replaces an existing file.
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const BRAND: &str = "Flect-for-Mac";
const ABANDONED_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Unique per process and call. Uniqueness is not a security boundary:
/// every creation below uses `create_new`, which fails if the name exists.
pub fn unique_token() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(
        "{:x}{:x}{:x}",
        std::process::id(),
        nanos,
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// A file being produced. Removed on drop unless [`WorkingFile::keep`] is called.
pub struct WorkingFile {
    path: PathBuf,
    keep: bool,
}

impl WorkingFile {
    pub fn new(path: PathBuf) -> Self {
        Self { path, keep: false }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn keep(&mut self) {
        self.keep = true;
    }
}

impl Drop for WorkingFile {
    fn drop(&mut self) {
        if !self.keep {
            safe_delete(&self.path);
        }
    }
}

pub fn safe_delete(path: &Path) {
    if path
        .symlink_metadata()
        .map(|m| !m.is_dir())
        .unwrap_or(false)
    {
        let _ = fs::remove_file(path);
    }
}

/// Publish `temp` as `target` without ever replacing an existing entry.
/// Uses a hard link (fails with `AlreadyExists`), then drops the temp name.
/// Where a link is impossible (another volume, or a file system without hard links, as
/// with an output folder on an external drive) the bytes are copied into a file created
/// exclusively at `target`, so an existing entry still cannot be replaced.
pub fn commit_new(temp: &Path, target: &Path) -> io::Result<()> {
    match fs::hard_link(temp, target) {
        Ok(()) => fs::remove_file(temp),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::AlreadyExists | io::ErrorKind::NotFound
            ) =>
        {
            Err(e)
        }
        Err(_) => {
            let mut src = fs::File::open(temp)?;
            let mut dst = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(target)?;
            let copied = io::copy(&mut src, &mut dst).and_then(|_| dst.sync_all());
            drop(dst);
            if let Err(e) = copied {
                let _ = fs::remove_file(target);
                return Err(e);
            }
            fs::remove_file(temp)
        }
    }
}

/// Hidden working directory beside a destination; deleted on drop.
pub struct ScratchDirectory {
    root: PathBuf,
}

impl ScratchDirectory {
    pub fn new(beside: &Path) -> io::Result<Self> {
        let parent = match beside.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => PathBuf::from("."),
        };
        sweep_abandoned(&parent);
        let root = parent.join(format!(".{BRAND}-{}", unique_token()));
        fs::create_dir(&root)?; // fails if it exists
        Ok(Self { root })
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn path_for(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
}

impl Drop for ScratchDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Remove scratch directories left by crashed runs (older than a day).
fn sweep_abandoned(parent: &Path) {
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    let prefix = format!(".{BRAND}-");
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with(&prefix) {
            continue;
        }
        let Ok(meta) = entry.path().symlink_metadata() else {
            continue;
        };
        if !meta.is_dir() {
            continue;
        }
        let old = meta
            .modified()
            .ok()
            .and_then(|m| SystemTime::now().duration_since(m).ok())
            .is_some_and(|age| age > ABANDONED_AFTER);
        if old {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn working_file_removed_unless_kept() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        fs::write(&a, b"1").unwrap();
        fs::write(&b, b"1").unwrap();
        drop(WorkingFile::new(a.clone()));
        let mut keep = WorkingFile::new(b.clone());
        keep.keep();
        drop(keep);
        assert!(!a.exists());
        assert!(b.exists());
    }

    #[test]
    fn commit_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let (t, f) = (dir.path().join("t"), dir.path().join("final"));
        fs::write(&t, b"new").unwrap();
        fs::write(&f, b"old").unwrap();
        assert_eq!(
            commit_new(&t, &f).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(&f).unwrap(), b"old");
        fs::remove_file(&f).unwrap();
        commit_new(&t, &f).unwrap();
        assert_eq!(fs::read(&f).unwrap(), b"new");
        assert!(!t.exists());
    }

    #[test]
    fn scratch_directory_cleans_up_and_sweeps_old() {
        let dir = tempfile::tempdir().unwrap();
        let root = {
            let s = ScratchDirectory::new(&dir.path().join("out.bin")).unwrap();
            fs::write(s.path_for("x"), b"1").unwrap();
            s.root().to_path_buf()
        };
        assert!(!root.exists());

        let stale = dir.path().join(format!(".{BRAND}-stale"));
        fs::create_dir(&stale).unwrap();
        let old = SystemTime::now() - Duration::from_secs(3 * 24 * 3600);
        fs::File::open(&stale).unwrap().set_modified(old).unwrap();
        let _s = ScratchDirectory::new(&dir.path().join("out.bin")).unwrap();
        assert!(!stale.exists());
    }

    #[test]
    fn tokens_differ() {
        assert_ne!(unique_token(), unique_token());
    }
}
