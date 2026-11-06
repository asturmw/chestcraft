//! File operations below a volume root: creating folders, moving entries without ever
//! overwriting, measuring and removing trees. Nothing here follows a symlink in the last
//! path component or crosses into another filesystem.
//!
//! chestcraft's own working folders (`.chestcraft-trash`, `.chestcraft-uploads`) sit in the
//! "filesystem root" of a folder: the highest folder above it, inside the volume, that is
//! still on the same filesystem (ZFS dataset). Moves between them and the volume's folders
//! are then plain renames, instant even for large files.

use crate::db::DbError;
use crate::fs::names::{self, Variant};
use crate::fs::path::{PathError, RelPath};
use crate::fs::volume::VolumeRoot;
use rustix::fs::{
    AtFlags, Dir, FileType, Mode, OFlags, RenameFlags, Stat, fchmod, fstat, mkdirat, openat,
    renameat_with, statat, unlinkat,
};
use rustix::io::Errno;
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

pub const TRASH_DIR: &str = ".chestcraft-trash";
pub const UPLOADS_DIR: &str = ".chestcraft-uploads";

/// Deepest folder nesting chestcraft walks into.
const MAX_DEPTH: usize = 1024;

/// What an existing entry is, for name-clash answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Existing {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum OpError {
    #[error("not found")]
    NotFound,
    #[error("\"{}\" exists already", .0.name)]
    Exists(Existing),
    #[error("\"{0}\" is in the way: it isn't a folder")]
    Blocked(String),
    #[error("not enough free space")]
    NoSpace,
    #[error("contains a read-only area")]
    ReadOnlyInside,
    #[error("is or contains another volume")]
    VolumeInside,
    #[error("not allowed")]
    Denied,
    #[error("is or contains another filesystem")]
    MountPoint,
    #[error("can't use {path}: {source}")]
    WorkDir {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("busy")]
    Busy,
    #[error("not all chunks have arrived")]
    Incomplete,
    #[error("gone")]
    Gone,
    #[error("invalid request: {0}")]
    Invalid(&'static str),
    #[error("{0}")]
    Io(io::Error),
    #[error("{0}")]
    Db(#[from] DbError),
}

impl From<io::Error> for OpError {
    fn from(e: io::Error) -> Self {
        match e.kind() {
            io::ErrorKind::NotFound => OpError::NotFound,
            io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded => OpError::NoSpace,
            io::ErrorKind::CrossesDevices => OpError::MountPoint,
            _ => OpError::Io(e),
        }
    }
}

impl From<Errno> for OpError {
    fn from(e: Errno) -> Self {
        io::Error::from(e).into()
    }
}

impl From<PathError> for OpError {
    fn from(_: PathError) -> Self {
        OpError::Invalid("path")
    }
}

/// The OS error number behind an I/O error, if any.
pub fn errno(e: &io::Error) -> Option<Errno> {
    e.raw_os_error().map(Errno::from_raw_os_error)
}

/// Kind of a directory entry, without following symlinks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Other,
}

pub fn lstat(dir: impl AsFd, name: &str) -> io::Result<(EntryKind, Stat)> {
    let st = statat(dir, name, AtFlags::SYMLINK_NOFOLLOW)?;
    let kind = match FileType::from_raw_mode(st.st_mode as _) {
        FileType::RegularFile => EntryKind::File,
        FileType::Directory => EntryKind::Dir,
        FileType::Symlink => EntryKind::Symlink,
        _ => EntryKind::Other,
    };
    Ok((kind, st))
}

// rustix's `Stat` field types differ per architecture; the casts are no-ops on x86_64.
#[allow(clippy::unnecessary_cast)]
pub fn dev_of(st: &Stat) -> u64 {
    st.st_dev as u64
}

pub fn fd_dev(fd: impl AsFd) -> io::Result<u64> {
    Ok(dev_of(&fstat(fd)?))
}

#[allow(clippy::unnecessary_cast)]
pub fn existing(name: &str, kind: EntryKind, st: &Stat) -> Existing {
    Existing {
        name: name.to_string(),
        is_dir: kind == EntryKind::Dir,
        size: st.st_size as u64,
        mtime: st.st_mtime as i64,
    }
}

/// Open a directory entry without following a symlink.
pub fn open_subdir(dir: impl AsFd, name: &str) -> io::Result<OwnedFd> {
    Ok(openat(
        dir,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?)
}

/// Random id for trash entries, uploads and batches.
pub fn new_id() -> String {
    crate::auth::random::hex(&crate::auth::random::bytes::<8>())
}

/// The filesystem root of `dir`: the highest folder from the volume root down to `dir`
/// such that it and every folder below it on the way are on `dir`'s filesystem.
pub fn fs_root(vol: &VolumeRoot, dir: &RelPath) -> io::Result<RelPath> {
    let mut prefixes = vec![RelPath::root()];
    for c in dir.components() {
        let next = prefixes
            .last()
            .expect("non-empty")
            .join(c)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        prefixes.push(next);
    }
    let devs = prefixes
        .iter()
        .map(|p| vol.stat(p).map(|m| m.dev))
        .collect::<io::Result<Vec<u64>>>()?;
    let target = *devs.last().expect("non-empty");
    let mut top = devs.len() - 1;
    while top > 0 && devs[top - 1] == target {
        top -= 1;
    }
    Ok(prefixes.swap_remove(top))
}

/// Open (creating it if needed) one of chestcraft's working folders in `fs_root`.
pub fn work_dir(vol: &VolumeRoot, fs_root: &RelPath, which: &str) -> Result<OwnedFd, OpError> {
    let label = || format!("{}{}/{which}", vol.cfg.path.display(), fs_root);
    let wrap = |source: io::Error| OpError::WorkDir {
        path: label(),
        source,
    };
    let base = vol.open_dir(fs_root).map_err(wrap)?;
    let fd = match open_subdir(&base, which) {
        Ok(fd) => fd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            match mkdirat(&base, which, Mode::from_raw_mode(0o700)) {
                Ok(()) | Err(Errno::EXIST) => {}
                Err(e) => return Err(wrap(e.into())),
            }
            open_subdir(&base, which).map_err(wrap)?
        }
        Err(e) => return Err(wrap(e)),
    };
    if fd_dev(&fd).map_err(wrap)? != fd_dev(&base).map_err(wrap)? {
        return Err(wrap(io::Error::from(io::ErrorKind::CrossesDevices)));
    }
    Ok(fd)
}

/// Create one folder (mode 755) in `dir`.
pub fn mkdir_in(dir: impl AsFd, name: &str) -> Result<(), OpError> {
    let dir = dir.as_fd();
    match mkdirat(dir, name, Mode::from_raw_mode(0o755)) {
        Ok(()) => {
            // Don't depend on the service's umask.
            let fd = open_subdir(dir, name)?;
            fchmod(&fd, Mode::from_raw_mode(0o755))?;
            Ok(())
        }
        Err(Errno::EXIST) => {
            let (kind, st) = lstat(dir, name)?;
            Err(OpError::Exists(existing(name, kind, &st)))
        }
        Err(e) => Err(e.into()),
    }
}

/// Make sure the folder `rel` exists in the volume, creating missing parts (mode 755), and
/// open it. Existing folders may be symlinks that stay inside the volume.
pub fn mkdirs(vol: &VolumeRoot, rel: &RelPath) -> Result<OwnedFd, OpError> {
    let mut fd = vol.open_dir(&RelPath::root())?;
    let mut cur = RelPath::root();
    for c in rel.components() {
        cur = cur.join(c)?;
        match mkdir_in(&fd, c) {
            Ok(()) | Err(OpError::Exists(_)) => {}
            Err(e) => return Err(e),
        }
        fd = match vol.open_dir(&cur) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(OpError::Blocked(c.to_string()));
            }
            Err(e) => return Err(e.into()),
        };
    }
    Ok(fd)
}

/// Like [`mkdirs`], but below a directory chestcraft owns (no symlinks followed).
pub fn mkdirs_at(base: &OwnedFd, rel: &RelPath) -> Result<OwnedFd, OpError> {
    let mut fd = base.try_clone()?;
    for c in rel.components() {
        match mkdir_in(&fd, c) {
            Ok(()) | Err(OpError::Exists(_)) => {}
            Err(e) => return Err(e),
        }
        fd = match open_subdir(&fd, c) {
            Ok(f) => f,
            Err(e) if matches!(errno(&e), Some(Errno::NOTDIR | Errno::LOOP)) => {
                return Err(OpError::Blocked(c.to_string()));
            }
            Err(e) => return Err(e.into()),
        };
    }
    Ok(fd)
}

/// Rename without replacing anything at the destination.
pub fn rename_noreplace(
    src_dir: impl AsFd,
    src: &str,
    dst_dir: impl AsFd,
    dst: &str,
) -> io::Result<()> {
    Ok(renameat_with(
        src_dir,
        src,
        dst_dir,
        dst,
        RenameFlags::NOREPLACE,
    )?)
}

/// Swap two entries atomically.
pub fn exchange(a_dir: impl AsFd, a: &str, b_dir: impl AsFd, b: &str) -> io::Result<()> {
    Ok(renameat_with(a_dir, a, b_dir, b, RenameFlags::EXCHANGE)?)
}

/// Move `src` to `name` in `dst_dir`, or to the first free alternative name. Returns the
/// name used.
pub fn place_unique(
    src_dir: impl AsFd,
    src: &str,
    dst_dir: impl AsFd,
    name: &str,
    is_dir: bool,
    variant: Variant,
) -> io::Result<String> {
    let (src_dir, dst_dir) = (src_dir.as_fd(), dst_dir.as_fd());
    for n in 0..10_000u32 {
        let candidate = if n == 0 {
            name.to_string()
        } else {
            names::alternative(name, is_dir, variant, n)
        };
        match rename_noreplace(src_dir, src, dst_dir, &candidate) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other("no free name found"))
}

/// Names in a directory, without `.` and `..`.
fn entries(dir: &OwnedFd) -> io::Result<Vec<String>> {
    let mut out = Vec::new();
    let mut d = Dir::read_from(dir)?;
    while let Some(ent) = d.read() {
        let ent = ent?;
        let bytes = ent.file_name().to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        // Names that aren't UTF-8 still count; they're carried along by renames.
        out.push(String::from_utf8_lossy(bytes).into_owned());
    }
    Ok(out)
}

/// Raw directory entries (bytes), for removal, which must handle any name.
fn raw_entries(dir: &OwnedFd) -> io::Result<Vec<Vec<u8>>> {
    let mut out = Vec::new();
    let mut d = Dir::read_from(dir)?;
    while let Some(ent) = d.read() {
        let ent = ent?;
        let bytes = ent.file_name().to_bytes();
        if bytes != b"." && bytes != b".." {
            out.push(bytes.to_vec());
        }
    }
    Ok(out)
}

/// Top-level names in a directory chestcraft owns.
pub fn list_names(dir: &OwnedFd) -> io::Result<Vec<String>> {
    entries(dir)
}

/// Total size and file count of the tree at `name` in `dir`. Fails with `CrossesDevices`
/// when a folder in it is on another filesystem than `dev` (a mount point).
#[allow(clippy::unnecessary_cast)]
pub fn scan_tree(dir: impl AsFd, name: &str, dev: u64) -> io::Result<(u64, u64)> {
    fn walk(dir: &OwnedFd, dev: u64, depth: usize, acc: &mut (u64, u64)) -> io::Result<()> {
        if depth > MAX_DEPTH {
            return Err(io::Error::other("folders nested too deeply"));
        }
        for name in raw_entries(dir)? {
            let st = statat(dir, name.as_slice(), AtFlags::SYMLINK_NOFOLLOW)?;
            match FileType::from_raw_mode(st.st_mode as _) {
                FileType::Directory => {
                    if dev_of(&st) != dev {
                        return Err(io::ErrorKind::CrossesDevices.into());
                    }
                    let sub = openat(
                        dir,
                        name.as_slice(),
                        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                        Mode::empty(),
                    )?;
                    walk(&sub, dev, depth + 1, acc)?;
                }
                _ => {
                    acc.0 += st.st_size as u64;
                    acc.1 += 1;
                }
            }
        }
        Ok(())
    }
    let (kind, st) = lstat(&dir, name)?;
    if kind != EntryKind::Dir {
        return Ok((st.st_size as u64, 1));
    }
    if dev_of(&st) != dev {
        return Err(io::ErrorKind::CrossesDevices.into());
    }
    let sub = open_subdir(&dir, name)?;
    let mut acc = (0, 0);
    walk(&sub, dev, 1, &mut acc)?;
    Ok(acc)
}

/// Remove the tree at `name` in `dir` for good. Never follows symlinks and never enters a
/// folder on another filesystem than `dev` (fails with `CrossesDevices` instead).
pub fn remove_tree(dir: impl AsFd, name: &str, dev: u64) -> io::Result<()> {
    #[allow(clippy::unnecessary_cast)]
    fn remove(dir: BorrowedFd<'_>, name: &[u8], dev: u64, depth: usize) -> io::Result<()> {
        if depth > MAX_DEPTH {
            return Err(io::Error::other("folders nested too deeply"));
        }
        let st = statat(dir, name, AtFlags::SYMLINK_NOFOLLOW)?;
        if FileType::from_raw_mode(st.st_mode as _) != FileType::Directory {
            return Ok(unlinkat(dir, name, AtFlags::empty())?);
        }
        if dev_of(&st) != dev {
            return Err(io::ErrorKind::CrossesDevices.into());
        }
        let sub = openat(
            dir,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        for child in raw_entries(&sub)? {
            remove(sub.as_fd(), &child, dev, depth + 1)?;
        }
        Ok(unlinkat(dir, name, AtFlags::REMOVEDIR)?)
    }
    remove(dir.as_fd(), name.as_bytes(), dev, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::path::Path;

    fn volume(dir: &Path) -> VolumeRoot {
        let cfg = Config::from_toml_str(&format!(
            "[server]\npublic_url = \"http://x.org\"\n[volumes.T]\npath = \"{}\"\n",
            dir.display()
        ))
        .unwrap();
        VolumeRoot::open(cfg.volumes["T"].clone()).unwrap()
    }

    fn p(s: &str) -> RelPath {
        RelPath::parse(s).unwrap()
    }

    #[test]
    fn mkdirs_creates_missing_folders_and_reports_files_in_the_way() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("a")).unwrap();
        std::fs::write(tmp.path().join("a/file"), "x").unwrap();
        let v = volume(tmp.path());
        mkdirs(&v, &p("a/b/c")).unwrap();
        assert!(tmp.path().join("a/b/c").is_dir());
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(tmp.path().join("a/b/c"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
        assert!(matches!(mkdirs(&v, &p("a/file/x")), Err(OpError::Blocked(n)) if n == "file"));
        assert!(matches!(
            mkdir_in(v.open_dir(&p("a")).unwrap(), "b"),
            Err(OpError::Exists(e)) if e.is_dir
        ));
    }

    #[test]
    fn fs_root_of_a_plain_tree_is_the_volume_root() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("x/y")).unwrap();
        let v = volume(tmp.path());
        assert_eq!(fs_root(&v, &p("x/y")).unwrap(), RelPath::root());
        let w = work_dir(&v, &RelPath::root(), TRASH_DIR).unwrap();
        assert!(tmp.path().join(TRASH_DIR).is_dir());
        drop(w);
        // Opening again finds the existing folder.
        work_dir(&v, &RelPath::root(), TRASH_DIR).unwrap();
    }

    #[test]
    fn place_unique_never_overwrites() {
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path();
        std::fs::write(r.join("Angebot.pdf"), "old").unwrap();
        std::fs::write(r.join("Angebot (1).pdf"), "old1").unwrap();
        std::fs::write(r.join("new"), "new").unwrap();
        let v = volume(r);
        let d = v.open_dir(&RelPath::root()).unwrap();
        let used = place_unique(&d, "new", &d, "Angebot.pdf", false, Variant::Numbered).unwrap();
        assert_eq!(used, "Angebot (2).pdf");
        assert_eq!(std::fs::read(r.join("Angebot.pdf")).unwrap(), b"old");
        assert_eq!(std::fs::read(r.join("Angebot (2).pdf")).unwrap(), b"new");
    }

    #[test]
    fn scan_and_remove_do_not_follow_symlinks() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("keep.txt"), "keep").unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path();
        std::fs::create_dir_all(r.join("t/sub")).unwrap();
        std::fs::write(r.join("t/a.bin"), vec![0u8; 100]).unwrap();
        std::fs::write(r.join("t/sub/b.bin"), vec![0u8; 23]).unwrap();
        std::os::unix::fs::symlink(outside.path(), r.join("t/link")).unwrap();
        let v = volume(r);
        let d = v.open_dir(&RelPath::root()).unwrap();
        let dev = fd_dev(&d).unwrap();
        let (size, files) = scan_tree(&d, "t", dev).unwrap();
        assert_eq!(files, 3, "two files and the link itself");
        assert!(size >= 123);
        remove_tree(&d, "t", dev).unwrap();
        assert!(!r.join("t").exists());
        assert!(outside.path().join("keep.txt").exists());
        assert_eq!(
            remove_tree(&d, "t", dev).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn foreign_filesystems_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("t")).unwrap();
        let v = volume(tmp.path());
        let d = v.open_dir(&RelPath::root()).unwrap();
        let wrong_dev = fd_dev(&d).unwrap() ^ 1;
        assert_eq!(
            scan_tree(&d, "t", wrong_dev).unwrap_err().kind(),
            io::ErrorKind::CrossesDevices
        );
        assert_eq!(
            remove_tree(&d, "t", wrong_dev).unwrap_err().kind(),
            io::ErrorKind::CrossesDevices
        );
        assert!(tmp.path().join("t").is_dir());
    }
}
