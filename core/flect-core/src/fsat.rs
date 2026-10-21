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
