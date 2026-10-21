//! File system access through directory handles (`openat`, `fstatat`, `unlinkat`), for the steps that
//! remove user files. A path is only a way to find a directory once: everything after that happens
//! relative to an open, verified directory, so renaming or swapping a folder above it (`mv A A.old;
//! mv B A`, a link put in its place, a sync client) cannot redirect a removal to another file.
use std::fs::{self, File};
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path};

use rustix::fs::{self as rfs, AtFlags, Mode, OFlags};

/// (device, inode).
pub type Id = (u64, u64);

/// What identifies a file's state: device, inode, size, modification and status change time (s, ns).
/// Two looks that agree on all of it saw the same file with the same contents, as far as the file
/// system can tell. As text (`Display`) it is the protocol's `fingerprint` (see
/// `docs/mac-port/08-engine-protocol.ja.md`): `f:dev:ino:size:mtime_s.mtime_ns:ctime_s.ctime_ns`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Fingerprint {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime: (i64, i64),
    pub ctime: (i64, i64),
}

impl Fingerprint {
    pub fn of_metadata(m: &fs::Metadata) -> Self {
        Self {
            dev: m.dev(),
            ino: m.ino(),
            size: m.len(),
            mtime: (m.mtime(), m.mtime_nsec()),
            ctime: (m.ctime(), m.ctime_nsec()),
        }
    }

    #[allow(clippy::unnecessary_cast)] // the stat field types differ between platforms
    pub fn of_stat(st: &rfs::Stat) -> Self {
        Self {
            dev: st.st_dev as u64,
            ino: st.st_ino as u64,
            size: st.st_size as u64,
            mtime: (st.st_mtime as i64, st.st_mtime_nsec as i64),
            ctime: (st.st_ctime as i64, st.st_ctime_nsec as i64),
        }
    }

    pub fn id(&self) -> Id {
        (self.dev, self.ino)
    }
}

impl std::fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "f:{}:{}:{}:{}.{}:{}.{}",
            self.dev, self.ino, self.size, self.mtime.0, self.mtime.1, self.ctime.0, self.ctime.1
        )
    }
}

/// The protocol fingerprint of a folder: `d:dev:ino` (its size and times change with its entries).
pub fn dir_fingerprint(id: Id) -> String {
    format!("d:{}:{}", id.0, id.1)
}

fn kind(st: &rfs::Stat) -> rfs::FileType {
    rfs::FileType::from_raw_mode(st.st_mode as rfs::RawMode)
}

pub fn is_dir(st: &rfs::Stat) -> bool {
    kind(st) == rfs::FileType::Directory
}

pub fn is_file(st: &rfs::Stat) -> bool {
    kind(st) == rfs::FileType::RegularFile
}

fn dir_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

/// Open the folder `root` (the selected item as given; it must itself be a folder, not a link) and
/// check it is `root_id` when given, then each component of `rel` below it with
/// `O_DIRECTORY | O_NOFOLLOW`: a link anywhere below the root is refused, never followed. `rel` may
/// only hold plain names (no `..`, `.`, or root).
pub fn open_below(root: &Path, root_id: Option<Id>, rel: &Path) -> io::Result<OwnedFd> {
    let mut fd = rfs::openat(rfs::CWD, root, dir_flags(), Mode::empty())?;
    if let Some(want) = root_id {
        if Fingerprint::of_stat(&rfs::fstat(&fd)?).id() != want {
            return Err(changed("the selected folder is not the one scanned"));
        }
    }
    for c in rel.components() {
        let Component::Normal(name) = c else {
            return Err(changed("not a plain path below the selected folder"));
        };
        fd = rfs::openat(&fd, name, dir_flags(), Mode::empty())?;
    }
    Ok(fd)
}

/// An error meaning "this is not the item that was checked any more".
pub fn changed(what: &str) -> io::Error {
    io::Error::other(what.to_owned())
}

/// `fstatat(dir, name, AT_SYMLINK_NOFOLLOW)`.
pub fn stat_at(dir: &OwnedFd, name: &std::ffi::OsStr) -> io::Result<rfs::Stat> {
    Ok(rfs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW)?)
}

/// Open `name` in `dir` for reading, without following a link.
pub fn open_file_at(dir: &OwnedFd, name: &std::ffi::OsStr) -> io::Result<File> {
    let fd = rfs::openat(
        dir,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    Ok(File::from(fd))
}

/// Open the folder `name` in `dir`, without following a link.
pub fn open_dir_at(dir: &OwnedFd, name: &std::ffi::OsStr) -> io::Result<OwnedFd> {
    Ok(rfs::openat(dir, name, dir_flags(), Mode::empty())?)
}

/// `fstat` of an open file or folder.
pub fn stat_fd(fd: &impl std::os::fd::AsFd) -> io::Result<rfs::Stat> {
    Ok(rfs::fstat(fd)?)
}

/// `unlinkat(dir, name, 0)`: remove the file `name` from the open folder `dir`.
pub fn unlink_file_at(dir: &OwnedFd, name: &std::ffi::OsStr) -> io::Result<()> {
    Ok(rfs::unlinkat(dir, name, AtFlags::empty())?)
}

/// `unlinkat(dir, name, AT_REMOVEDIR)`: remove the empty folder `name` from the open folder `dir`.
pub fn remove_dir_at(dir: &OwnedFd, name: &std::ffi::OsStr) -> io::Result<()> {
    Ok(rfs::unlinkat(dir, name, AtFlags::REMOVEDIR)?)
}

/// The names in the open folder `dir` (without `.` and `..`).
pub fn names_in(dir: &OwnedFd) -> io::Result<Vec<std::ffi::OsString>> {
    use std::os::unix::ffi::OsStrExt;
    let mut out = Vec::new();
    for e in rfs::Dir::read_from(dir)? {
        let e = e?;
        let n = e.file_name().to_bytes();
        if n != b"." && n != b".." {
            out.push(std::ffi::OsStr::from_bytes(n).to_owned());
        }
    }
    Ok(out)
}

/// A unique, unpredictable name for an item that is about to be removed (`.flect-rm-…`), so the
/// removal can act on a name nobody else uses.
pub fn temp_name() -> std::ffi::OsString {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    h.write_u32(std::process::id());
    let a = h.finish();
    h.write_u64(a);
    format!(".flect-rm-{:016x}{:016x}", a, h.finish()).into()
}

/// Rename `from` to `to` inside the open folder `dir`, never replacing an existing `to`
/// (`renameatx_np(RENAME_EXCL)` / `renameat2(RENAME_NOREPLACE)`). Where the file system does not
/// support that flag (some FAT, exFAT or network volumes), falls back to a plain `renameat` after
/// checking that `to` does not exist; `to` is a fresh random name, so nothing else uses it.
pub fn rename_noreplace(
    dir: &OwnedFd,
    from: &std::ffi::OsStr,
    to: &std::ffi::OsStr,
) -> io::Result<()> {
    use rustix::io::Errno;
    match rfs::renameat_with(dir, from, dir, to, rfs::RenameFlags::NOREPLACE) {
        Err(e) if [Errno::INVAL, Errno::NOTSUP, Errno::NOSYS, Errno::OPNOTSUPP].contains(&e) => {
            if rfs::statat(dir, to, AtFlags::SYMLINK_NOFOLLOW).is_ok() {
                return Err(io::Error::from(Errno::EXIST));
            }
            Ok(rfs::renameat(dir, from, dir, to)?)
        }
        r => Ok(r?),
    }
}

/// How a removal step ended when it did not remove anything.
#[derive(Debug)]
pub enum NotRemoved {
    /// The name is gone already (nothing to do).
    Gone,
    /// What is there now is not what was checked; it was put back under its name.
    Changed,
    /// Not what was checked, and its name is taken now: left under `temp` in the same folder.
    LeftAs(std::ffi::OsString),
    Failed(io::Error),
}

/// Remove `name` from the open folder `dir` only if it still is what was checked, closing the gap
/// between the last check and the removal: the name is first renamed to a fresh temporary name
/// (never replacing anything), the renamed item is looked at again (`ok`, given its `fstatat`),
/// `last` runs (a final check of something else, such as the copy that stays), and only then the
/// temporary name is removed (`unlinkat`, or `AT_REMOVEDIR` for a folder). Whatever another
/// process renames into `name` after that first rename is never touched. If anything does not
/// hold, the item is renamed back (again without replacing) or, when its name is taken, left
/// under the temporary name and reported.
pub fn remove_checked(
    dir: &OwnedFd,
    name: &std::ffi::OsStr,
    folder: bool,
    ok: &dyn Fn(&rfs::Stat) -> bool,
    last: &dyn Fn() -> bool,
) -> Result<(), NotRemoved> {
    let temp = temp_name();
    seam("R1", Path::new(name));
    match rename_noreplace(dir, name, &temp) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(NotRemoved::Gone),
        Err(e) => return Err(NotRemoved::Failed(e)),
    }
    let put_back = |why: NotRemoved| match rename_noreplace(dir, &temp, name) {
        Ok(()) => why,
        Err(_) => NotRemoved::LeftAs(temp.clone()),
    };
    let still = rfs::statat(dir, &temp, AtFlags::SYMLINK_NOFOLLOW)
        .is_ok_and(|st| (if folder { is_dir(&st) } else { is_file(&st) }) && ok(&st));
    seam("R2", Path::new(name));
    if !still || !last() {
        return Err(put_back(NotRemoved::Changed));
    }
    let flags = if folder {
        AtFlags::REMOVEDIR
    } else {
        AtFlags::empty()
    };
    match rfs::unlinkat(dir, &temp, flags) {
        Ok(()) => Ok(()),
        Err(e) => Err(put_back(NotRemoved::Failed(e.into()))),
    }
}

/// Pause points for tests and reviews, named like the review harness's (`D1`-`D4` duplicates,
/// `E1`-`E3` Remove Empty, `T1` Trash hand-off, `R1`/`R2` around the temporary rename). In unit
/// tests a hook can be set per thread (`with_seam`); with the `review-seams` feature
/// `FLECT_SEAM_DIR` / `FLECT_SEAM` make the engine write `<dir>/ready` and wait for `<dir>/go`.
/// Otherwise a no-op.
#[allow(unused_variables)]
pub fn seam(phase: &str, p: &Path) {
    #[cfg(test)]
    SEAM.with(|h| {
        if let Some(f) = h.borrow().as_ref() {
            f(phase, p)
        }
    });
    #[cfg(feature = "review-seams")]
    {
        let (Ok(dir), Ok(list)) = (std::env::var("FLECT_SEAM_DIR"), std::env::var("FLECT_SEAM"))
        else {
            return;
        };
        if !list.split(',').any(|x| x == phase) {
            return;
        }
        let dir = Path::new(&dir);
        let _ = fs::write(dir.join("ready"), format!("{phase} {}", p.display()));
        let go = dir.join("go");
        while !go.exists() {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let _ = fs::remove_file(&go);
        let _ = fs::remove_file(dir.join("ready"));
    }
}

#[cfg(test)]
type SeamHook = Box<dyn Fn(&str, &Path)>;

#[cfg(test)]
thread_local! {
    static SEAM: std::cell::RefCell<Option<SeamHook>> = const { std::cell::RefCell::new(None) };
}

/// Run `f` with `hook` called at every [`seam`] on this thread.
#[cfg(test)]
pub fn with_seam<R>(hook: impl Fn(&str, &Path) + 'static, f: impl FnOnce() -> R) -> R {
    SEAM.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
    let r = f();
    SEAM.with(|h| *h.borrow_mut() = None);
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_below_refuses_links_and_swapped_roots() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        fs::create_dir_all(r.join("A/sub")).unwrap();
        fs::create_dir_all(r.join("B")).unwrap();
        std::os::unix::fs::symlink(r.join("B"), r.join("A/link")).unwrap();
        let id = |p: &Path| {
            let m = fs::symlink_metadata(p).unwrap();
            (m.dev(), m.ino())
        };
        let a = r.join("A");
        let fd = open_below(&a, Some(id(&a)), Path::new("sub")).unwrap();
        assert_eq!(
            Fingerprint::of_stat(&stat_fd(&fd).unwrap()).id(),
            id(&a.join("sub"))
        );
        assert!(
            open_below(&a, Some(id(&a)), Path::new("link")).is_err(),
            "link below the root"
        );
        assert!(
            open_below(&a, Some(id(&r.join("B"))), Path::new("")).is_err(),
            "other root"
        );
        assert!(open_below(&a, None, Path::new("../B")).is_err());
        assert!(
            open_below(&r.join("A/link"), None, Path::new("")).is_err(),
            "root itself a link"
        );
        // Display is the protocol fingerprint
        let m = fs::symlink_metadata(&a).unwrap();
        let fp = Fingerprint::of_metadata(&m);
        assert_eq!(fp, Fingerprint::of_stat(&rfs::lstat(&a).unwrap()));
        assert!(fp
            .to_string()
            .starts_with(&format!("f:{}:{}:", m.dev(), m.ino())));
        assert_eq!(dir_fingerprint((1, 2)), "d:1:2");
        assert_eq!(names_in(&fd).unwrap().len(), 0);
    }
}
