//! Moving and copying entries and writing new files, the primitives behind WebDAV (and later
//! the UI's rename/move/copy and the MCP tools).
//!
//! Nothing here overwrites by accident: every placement says what happens when the name is
//! taken ([`OnClash`]). New content is built in a temporary entry in the destination's
//! `.chestcraft-uploads` folder and renamed into place once complete and synced, so readers
//! (and the Transfer sorter) never see half a file or half a copied folder. Temporary entries
//! are recorded in the database first, so leftovers from a crash are found and removed.
//!
//! Moves are renames. A move across filesystems (another ZFS dataset, another volume's bind
//! mount) becomes a copy followed by moving the source to the trash: user files only leave
//! for good through the trash purge.
//!
//! Callers check permissions. Every call blocks; async code runs them in `spawn_blocking`.

use crate::db::{Db, TempRecord, TrashItem};
use crate::fs::names::Variant;
use crate::fs::ops::{self, EntryKind, OpError, UPLOADS_DIR};
use crate::fs::path::{RelPath, is_internal_name};
use crate::fs::trash;
use crate::fs::volume::{Kind, VolumeRoot, Volumes};
use rustix::fs::{
    AtFlags, Dir, FileType, Mode, OFlags, Stat, Timespec, Timestamps, UTIME_NOW, fchmod, fdatasync,
    fstat, fstatvfs, fsync, futimens, mkdirat, openat, readlinkat, renameat, statat, symlinkat,
    syncfs,
};
use rustix::io::Errno;
use std::fs::File;
use std::io::{self, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Deepest folder nesting a copy walks into.
const MAX_DEPTH: usize = 1024;

/// What happens when the destination name is taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnClash {
    /// Change nothing and report the existing entry.
    Fail,
    /// Change nothing.
    Skip,
    /// Keep both: the new entry gets the first free `name (n)`.
    KeepBoth,
    /// The existing entry goes to the trash first.
    Trash,
    /// WebDAV, with the right to delete: a file replaces a file in place (the old one
    /// survives only in snapshots); anything else in the way goes to the trash first.
    Overwrite,
    /// WebDAV, without the right to delete: only an empty file may be replaced in place.
    ReplaceEmpty,
}

/// What happened to an entry that was in the way.
#[derive(Debug, Clone, PartialEq)]
pub enum Replaced {
    Nothing,
    InPlace,
    Trashed(Box<TrashItem>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Placed {
    /// `name` is the name used (differs from the one asked for with [`OnClash::KeepBoth`]).
    Done {
        name: String,
        replaced: Replaced,
    },
    Skipped,
}

fn mtime(sec: i64, nsec: i64) -> Timestamps {
    Timestamps {
        last_access: Timespec {
            tv_sec: 0,
            tv_nsec: UTIME_NOW,
        },
        last_modification: Timespec {
            tv_sec: sec,
            tv_nsec: nsec,
        },
    }
}

/// Modification time from Unix milliseconds.
pub fn mtime_ms(ms: i64) -> Timestamps {
    mtime(ms.div_euclid(1000), ms.rem_euclid(1000) * 1_000_000)
}

#[allow(clippy::unnecessary_cast)]
fn mtime_of(st: &Stat) -> Timestamps {
    mtime(st.st_mtime as i64, st.st_mtime_nsec as i64)
}

/// File mode for a copy: 644, or 755 if the original was executable by anyone.
#[allow(clippy::unnecessary_cast)]
fn file_mode(st: &Stat) -> Mode {
    Mode::from_raw_mode(if st.st_mode as u32 & 0o111 != 0 {
        0o755
    } else {
        0o644
    })
}

// ---------------------------------------------------------------------------------------
// Temporary entries

/// A temporary file or folder in an uploads folder, known to the database. Dropping it
/// before [`Temp::finish`] removes it.
struct Temp {
    db: Arc<Db>,
    rec: TempRecord,
    updir: OwnedFd,
    done: bool,
}

impl Temp {
    fn new(
        db: &Arc<Db>,
        vol: &VolumeRoot,
        fs_root: &RelPath,
        suffix: &str,
        now: i64,
    ) -> Result<Temp, OpError> {
        let updir = ops::work_dir(vol, fs_root, UPLOADS_DIR)?;
        let id = ops::new_id();
        let rec = TempRecord {
            name: format!("{id}.{suffix}"),
            id,
            volume: vol.name().to_string(),
            fs_root: fs_root.as_str().to_string(),
            created: now,
        };
        db.put_temp(&rec)?;
        Ok(Temp {
            db: db.clone(),
            rec,
            updir,
            done: false,
        })
    }

    fn name(&self) -> &str {
        &self.rec.name
    }

    /// The entry was renamed into place: only the record goes.
    fn finish(mut self) {
        self.done = true;
        let _ = self.db.delete_temp(&self.rec.id);
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let removed = ops::fd_dev(&self.updir)
            .and_then(|dev| ops::remove_tree(&self.updir, &self.rec.name, dev));
        match removed {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => {
                // The record stays, so the next sweep tries again.
                tracing::warn!(volume = %self.rec.volume, name = %self.rec.name, "can't remove a temporary entry: {e}");
                return;
            }
        }
        let _ = self.db.delete_temp(&self.rec.id);
    }
}

/// Remove temporary entries older than `max_age` seconds, or all of them with `None` (at
/// startup, when no request that made one is still running). Returns how many went.
pub fn sweep_temps(db: &Db, vols: &Volumes, max_age: Option<i64>, now: i64) -> usize {
    let mut removed = 0;
    for rec in db.temps().unwrap_or_default() {
        if max_age.is_some_and(|age| now - rec.created < age) {
            continue;
        }
        let gone = match vols.get(&rec.volume) {
            None => Ok(()),
            Some(vol) => RelPath::parse(&rec.fs_root)
                .and_then(|r| r.join(UPLOADS_DIR))
                .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
                .and_then(|dir| match vol.open_dir(&dir) {
                    Ok(updir) => {
                        let dev = ops::fd_dev(&updir)?;
                        ops::remove_tree(&updir, &rec.name, dev)
                    }
                    Err(e) => Err(e),
                }),
        };
        match gone {
            Err(e) if e.kind() != io::ErrorKind::NotFound => {
                tracing::warn!(volume = %rec.volume, name = %rec.name, "can't remove a temporary entry: {e}");
            }
            _ => {
                let _ = db.delete_temp(&rec.id);
                removed += 1;
            }
        }
    }
    removed
}

// ---------------------------------------------------------------------------------------
// Placing

/// Rename `src_name` in `src_dir` to `name` in `dst_dir` (the folder `dst_parent` of
/// `vol`, on the same mount), resolving a clash with `on_clash`.
#[allow(clippy::too_many_arguments)]
fn place(
    db: &Db,
    vol: &VolumeRoot,
    dst_parent: &RelPath,
    src_dir: BorrowedFd<'_>,
    src_name: &str,
    src_is_dir: bool,
    dst_dir: BorrowedFd<'_>,
    name: &str,
    on_clash: OnClash,
    actor: &str,
    now: i64,
) -> Result<Placed, OpError> {
    let mut trashed: Option<TrashItem> = None;
    // A few rounds, in case the name is taken and freed again meanwhile.
    for _ in 0..4 {
        match ops::rename_noreplace(src_dir, src_name, dst_dir, name) {
            Ok(()) => {
                let replaced = match trashed {
                    Some(item) => Replaced::Trashed(Box::new(item)),
                    None => Replaced::Nothing,
                };
                return Ok(Placed::Done {
                    name: name.to_string(),
                    replaced,
                });
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
        let (kind, st) = match ops::lstat(dst_dir, name) {
            Ok(x) => x,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        // Two names for the same entry (overlapping volumes): nothing to do, and trashing
        // the "destination" would trash the source.
        if let Ok((_, sst)) = ops::lstat(src_dir, src_name)
            && (ops::dev_of(&sst), sst.st_ino) == (ops::dev_of(&st), st.st_ino)
        {
            return Err(OpError::Invalid("source and destination are the same"));
        }
        let ex = ops::existing(name, kind, &st);
        let file_on_file = kind == EntryKind::File && !src_is_dir;
        match on_clash {
            OnClash::Fail => return Err(OpError::Exists(ex)),
            OnClash::Skip => return Ok(Placed::Skipped),
            OnClash::KeepBoth => {
                let used = ops::place_unique(
                    src_dir,
                    src_name,
                    dst_dir,
                    name,
                    src_is_dir,
                    Variant::Numbered,
                )?;
                return Ok(Placed::Done {
                    name: used,
                    replaced: Replaced::Nothing,
                });
            }
            OnClash::ReplaceEmpty if !(file_on_file && ex.size == 0) => {
                return Err(OpError::Exists(ex));
            }
            OnClash::ReplaceEmpty | OnClash::Overwrite if file_on_file => {
                renameat(src_dir, src_name, dst_dir, name)?;
                return Ok(Placed::Done {
                    name: name.to_string(),
                    replaced: Replaced::InPlace,
                });
            }
            OnClash::ReplaceEmpty | OnClash::Overwrite | OnClash::Trash => {
                if trashed.is_some() {
                    return Err(OpError::Busy);
                }
                let target = dst_parent.join(name)?;
                trashed = Some(trash::delete(
                    db,
                    vol,
                    &target,
                    actor,
                    trash::REASON_REPLACED,
                    now,
                )?);
            }
        }
    }
    Err(OpError::Busy)
}

/// Set the modification time (Unix ms) of `rel` itself (a symlink's own time, not its
/// target's).
pub fn set_mtime(vol: &VolumeRoot, rel: &RelPath, ms: i64) -> Result<(), OpError> {
    let times = mtime_ms(ms);
    match (rel.parent(), rel.name()) {
        (Some(parent), Some(name)) => {
            let pfd = vol.open_dir(&parent)?;
            rustix::fs::utimensat(&pfd, name, &times, AtFlags::SYMLINK_NOFOLLOW)?;
        }
        _ => futimens(vol.open_dir(rel)?, &times)?,
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// New files

/// A file being written: data goes to a temporary file next to the uploads, and
/// [`NewFile::commit`] puts it in place.
pub struct NewFile {
    db: Arc<Db>,
    vol: Arc<VolumeRoot>,
    parent: RelPath,
    name: String,
    temp: Temp,
    file: File,
    written: u64,
}

impl NewFile {
    /// Start the file `name` in the existing folder `parent`. With a known size, refuses
    /// right away when it can't fit.
    pub fn create(
        db: &Arc<Db>,
        vol: &Arc<VolumeRoot>,
        parent: &RelPath,
        name: &str,
        size: Option<u64>,
        now: i64,
    ) -> Result<NewFile, OpError> {
        vol.open_dir(parent)?;
        let fs_root = ops::fs_root(vol, parent)?;
        let temp = Temp::new(db, vol, &fs_root, "part", now)?;
        if let Some(size) = size {
            let space = fstatvfs(&temp.updir)?;
            if size > space.f_bavail.saturating_mul(space.f_frsize) {
                return Err(OpError::NoSpace);
            }
        }
        let fd = openat(
            &temp.updir,
            temp.name(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )?;
        Ok(NewFile {
            db: db.clone(),
            vol: vol.clone(),
            parent: parent.clone(),
            name: name.to_string(),
            temp,
            file: File::from(fd),
            written: 0,
        })
    }

    pub fn write_all(&mut self, data: &[u8]) -> Result<(), OpError> {
        self.file.write_all(data)?;
        self.written += data.len() as u64;
        Ok(())
    }

    pub fn written(&self) -> u64 {
        self.written
    }

    /// Sync the data, set mode 644 and the modification time (Unix ms; None keeps "now"),
    /// and put the file in place.
    pub fn commit(
        self,
        mtime: Option<i64>,
        on_clash: OnClash,
        actor: &str,
        now: i64,
    ) -> Result<Placed, OpError> {
        fdatasync(&self.file)?;
        fchmod(&self.file, Mode::from_raw_mode(0o644))?;
        if let Some(ms) = mtime {
            futimens(&self.file, &mtime_ms(ms))?;
        }
        let pfd = self.vol.open_dir(&self.parent)?;
        let placed = place(
            &self.db,
            &self.vol,
            &self.parent,
            self.temp.updir.as_fd(),
            self.temp.name(),
            false,
            pfd.as_fd(),
            &self.name,
            on_clash,
            actor,
            now,
        )?;
        if matches!(placed, Placed::Done { .. }) {
            let _ = fsync(&pfd);
            self.temp.finish();
        }
        Ok(placed)
    }
}

// ---------------------------------------------------------------------------------------
// Copying

#[derive(Debug, Clone, Copy, Default)]
pub struct CopySpec {
    /// Copy dot-names too (internal names never).
    pub include_hidden: bool,
    /// A folder without its contents (WebDAV `Depth: 0`).
    pub shallow: bool,
}

/// Copy the file or folder `src` of `src_vol` to `name` in the existing folder `dst_parent`
/// of `dst_vol`. Symlinks inside a copied folder are copied as links; devices and FIFOs
/// are left out. Modification times are kept; files get mode 644 (755 if executable),
/// folders 755.
#[allow(clippy::too_many_arguments)]
pub fn copy_entry(
    db: &Arc<Db>,
    src_vol: &VolumeRoot,
    src: &RelPath,
    dst_vol: &VolumeRoot,
    dst_parent: &RelPath,
    name: &str,
    spec: CopySpec,
    on_clash: OnClash,
    actor: &str,
    now: i64,
) -> Result<Placed, OpError> {
    let dpfd = dst_vol.open_dir(dst_parent)?;
    let fs_root = ops::fs_root(dst_vol, dst_parent)?;
    let sfd = src_vol.open_read(src)?;
    let st = fstat(&sfd)?;
    let is_dir = match FileType::from_raw_mode(st.st_mode as _) {
        FileType::Directory => true,
        FileType::RegularFile => false,
        _ => return Err(OpError::Invalid("not a file or folder")),
    };
    let temp = Temp::new(db, dst_vol, &fs_root, "copy", now)?;
    if is_dir {
        mkdirat(&temp.updir, temp.name(), Mode::from_raw_mode(0o700))?;
        let tfd = ops::open_subdir(&temp.updir, temp.name())?;
        if !spec.shallow {
            copy_children(&sfd, &tfd, spec.include_hidden, 1)?;
            syncfs(&tfd)?;
        }
        fchmod(&tfd, Mode::from_raw_mode(0o755))?;
        futimens(&tfd, &mtime_of(&st))?;
    } else {
        copy_file(&sfd, &st, &temp.updir, temp.name().as_bytes())?;
    }
    let placed = place(
        db,
        dst_vol,
        dst_parent,
        temp.updir.as_fd(),
        temp.name(),
        is_dir,
        dpfd.as_fd(),
        name,
        on_clash,
        actor,
        now,
    )?;
    if matches!(placed, Placed::Done { .. }) {
        let _ = fsync(&dpfd);
        temp.finish();
    }
    Ok(placed)
}

/// Copy an open regular file to `name` (new) in `dir`, synced.
fn copy_file(src: &OwnedFd, st: &Stat, dir: &OwnedFd, name: &[u8]) -> io::Result<()> {
    let out = openat(
        dir,
        name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )?;
    let mut out = File::from(out);
    let mut input = File::from(src.try_clone()?);
    // std uses copy_file_range here, so ZFS can clone blocks instead of copying them.
    io::copy(&mut input, &mut out)?;
    fdatasync(&out)?;
    fchmod(&out, file_mode(st))?;
    futimens(&out, &mtime_of(st))?;
    Ok(())
}

fn copy_children(
    src: &OwnedFd,
    dst: &OwnedFd,
    include_hidden: bool,
    depth: usize,
) -> io::Result<()> {
    if depth > MAX_DEPTH {
        return Err(io::Error::other("folders nested too deeply"));
    }
    let mut names = Vec::new();
    let mut d = Dir::read_from(src)?;
    while let Some(ent) = d.read() {
        let ent = ent?;
        let bytes = ent.file_name().to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        if std::str::from_utf8(bytes).is_ok_and(is_internal_name)
            || (!include_hidden && bytes.starts_with(b"."))
        {
            continue;
        }
        names.push(bytes.to_vec());
    }
    for name in names {
        let name = name.as_slice();
        let st = statat(src, name, AtFlags::SYMLINK_NOFOLLOW)?;
        match FileType::from_raw_mode(st.st_mode as _) {
            FileType::Directory => {
                let sub = openat(
                    src,
                    name,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )?;
                mkdirat(dst, name, Mode::from_raw_mode(0o700))?;
                let dsub = openat(
                    dst,
                    name,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )?;
                copy_children(&sub, &dsub, include_hidden, depth + 1)?;
                fchmod(&dsub, Mode::from_raw_mode(0o755))?;
                futimens(&dsub, &mtime_of(&st))?;
            }
            FileType::RegularFile => {
                let f = openat(
                    src,
                    name,
                    OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )?;
                // Synced all at once by the caller's syncfs.
                let out = openat(
                    dst,
                    name,
                    OFlags::WRONLY
                        | OFlags::CREATE
                        | OFlags::EXCL
                        | OFlags::NOFOLLOW
                        | OFlags::CLOEXEC,
                    Mode::from_raw_mode(0o600),
                )?;
                let mut out = File::from(out);
                io::copy(&mut File::from(f), &mut out)?;
                fchmod(&out, file_mode(&st))?;
                futimens(&out, &mtime_of(&st))?;
            }
            FileType::Symlink => {
                let target = readlinkat(src, name, Vec::new())?;
                symlinkat(target.as_c_str(), dst, name)?;
            }
            _ => {}
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Moving

/// The absolute path of an open file or folder.
fn real_path(fd: &OwnedFd) -> io::Result<PathBuf> {
    std::fs::read_link(format!("/proc/self/fd/{}", fd.as_raw_fd()))
}

/// Undo the octal escapes in `/proc/self/mountinfo` paths (`\040` = space).
fn unescape_mount_path(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && i + 3 < b.len()
            && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c))
        {
            let v = (b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0');
            out.push(v);
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Mount points in a mountinfo text at or below `dir`.
fn mounts_at_or_below(mountinfo: &str, dir: &Path) -> bool {
    mountinfo
        .lines()
        .filter_map(|l| l.split(' ').nth(4))
        .any(|mp| Path::new(&unescape_mount_path(mp)).starts_with(dir))
}

/// Is the folder `name` in `parent` a mount point, or does it contain one? Renaming it
/// would leave the mount's configuration pointing at a path that's gone.
fn holds_mount(parent: &OwnedFd, name: &str, st: &Stat) -> Result<bool, OpError> {
    if ops::dev_of(st) != ops::fd_dev(parent)? {
        return Ok(true);
    }
    let sub = ops::open_subdir(parent, name)?;
    let real = real_path(&sub)?;
    let info = std::fs::read_to_string("/proc/self/mountinfo")?;
    Ok(mounts_at_or_below(&info, &real))
}

/// Move `src` of `src_vol` to `name` in the existing folder `dst_parent` of `dst_vol`.
/// Never moves a volume root, read-only areas, other volumes or mount points. A move to
/// another filesystem copies everything (dotfiles too) and sends the source to the trash.
/// Shares of the entry (or of anything inside it) move along.
#[allow(clippy::too_many_arguments)]
pub fn move_entry(
    db: &Arc<Db>,
    src_vol: &VolumeRoot,
    src: &RelPath,
    dst_vol: &VolumeRoot,
    dst_parent: &RelPath,
    name: &str,
    on_clash: OnClash,
    actor: &str,
    now: i64,
) -> Result<Placed, OpError> {
    let placed = move_inner(
        db, src_vol, src, dst_vol, dst_parent, name, on_clash, actor, now,
    )?;
    if let Placed::Done { name, .. } = &placed {
        let dst = dst_parent.join(name)?;
        if let Err(e) = crate::shares::follow_move(db, src_vol.name(), src, dst_vol.name(), &dst) {
            tracing::error!(volume = %src_vol.name(), path = %src, "shares didn't follow a move: {e}");
        }
    }
    Ok(placed)
}

#[allow(clippy::too_many_arguments)]
fn move_inner(
    db: &Arc<Db>,
    src_vol: &VolumeRoot,
    src: &RelPath,
    dst_vol: &VolumeRoot,
    dst_parent: &RelPath,
    name: &str,
    on_clash: OnClash,
    actor: &str,
    now: i64,
) -> Result<Placed, OpError> {
    let (Some(sparent), Some(sname)) = (src.parent(), src.name()) else {
        return Err(OpError::Invalid("the volume itself can't be moved"));
    };
    if src_vol.name() == dst_vol.name() {
        let dst = dst_parent.join(name)?;
        if dst == *src {
            return Err(OpError::Invalid("source and destination are the same"));
        }
        if dst.starts_with(src) {
            return Err(OpError::Invalid("a folder can't go inside itself"));
        }
    }
    if src_vol.holds_read_only(src) {
        return Err(OpError::ReadOnlyInside);
    }
    if src_vol.holds_volume(src) {
        return Err(OpError::VolumeInside);
    }
    let spfd = src_vol.open_dir(&sparent)?;
    let (kind, st) = ops::lstat(&spfd, sname)?;
    match kind {
        EntryKind::Dir if holds_mount(&spfd, sname, &st)? => return Err(OpError::MountPoint),
        // Links that leave the volume aren't listed, so they can't be moved either.
        EntryKind::Symlink => {
            src_vol.stat(src)?;
        }
        _ => {}
    }
    let is_dir = kind == EntryKind::Dir;
    let dpfd = dst_vol.open_dir(dst_parent)?;
    let placed = match ops::rename_noreplace(&spfd, sname, &dpfd, name) {
        Ok(()) => Placed::Done {
            name: name.to_string(),
            replaced: Replaced::Nothing,
        },
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => place(
            db,
            dst_vol,
            dst_parent,
            spfd.as_fd(),
            sname,
            is_dir,
            dpfd.as_fd(),
            name,
            on_clash,
            actor,
            now,
        )?,
        Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {
            return move_across(
                db, src_vol, src, dst_vol, dst_parent, name, on_clash, actor, now,
            );
        }
        Err(e) if ops::errno(&e) == Some(Errno::INVAL) => {
            return Err(OpError::Invalid("a folder can't go inside itself"));
        }
        Err(e) => return Err(e.into()),
    };
    if matches!(placed, Placed::Done { .. }) {
        let _ = fsync(&dpfd);
        let _ = fsync(&spfd);
    }
    Ok(placed)
}

#[allow(clippy::too_many_arguments)]
fn move_across(
    db: &Arc<Db>,
    src_vol: &VolumeRoot,
    src: &RelPath,
    dst_vol: &VolumeRoot,
    dst_parent: &RelPath,
    name: &str,
    on_clash: OnClash,
    actor: &str,
    now: i64,
) -> Result<Placed, OpError> {
    if src_vol.stat(src)?.kind == Kind::Other {
        return Err(OpError::Invalid("not a file or folder"));
    }
    let spec = CopySpec {
        include_hidden: true,
        shallow: false,
    };
    let placed = copy_entry(
        db, src_vol, src, dst_vol, dst_parent, name, spec, on_clash, actor, now,
    )?;
    if matches!(placed, Placed::Done { .. }) {
        // If this fails, both copies stay: nothing is lost.
        trash::delete(db, src_vol, src, actor, trash::REASON_MOVED, now)?;
    }
    Ok(placed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::db::FILE_NAME;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    struct T {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        vols: Volumes,
        db: Arc<Db>,
    }

    fn setup() -> T {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        for d in ["a/Fotos/2025", "a/.git", "b", "a/immich"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(root.join("a/Fotos/2025/x.jpg"), vec![7u8; 100]).unwrap();
        std::fs::write(root.join("a/Fotos/notes.txt"), "n").unwrap();
        std::fs::write(root.join("a/Fotos/run.sh"), "#!/bin/sh").unwrap();
        std::fs::set_permissions(
            root.join("a/Fotos/run.sh"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        std::fs::write(root.join("a/.git/HEAD"), "ref").unwrap();
        std::fs::write(root.join("a/Angebot.pdf"), "v1").unwrap();
        std::fs::write(root.join("b/Angebot.pdf"), "old").unwrap();
        std::fs::write(root.join("b/empty.txt"), "").unwrap();
        std::os::unix::fs::symlink("2025/x.jpg", root.join("a/Fotos/link")).unwrap();
        let mtime = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
        File::options()
            .write(true)
            .open(root.join("a/Fotos/2025/x.jpg"))
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        let cfg = Config::from_toml_str(&format!(
            "[server]\npublic_url = \"http://x.org\"\n\
             [volumes.A]\npath = \"{0}/a\"\nread_only_paths = [\"immich\"]\n\
             [volumes.B]\npath = \"{0}/b\"\n",
            root.display()
        ))
        .unwrap();
        let vols = Volumes::open(&cfg);
        let db = Arc::new(Db::open(&root.join(FILE_NAME)).unwrap());
        T {
            _tmp: tmp,
            root,
            vols,
            db,
        }
    }

    fn p(s: &str) -> RelPath {
        RelPath::parse(s).unwrap()
    }

    fn done(name: &str) -> Placed {
        Placed::Done {
            name: name.into(),
            replaced: Replaced::Nothing,
        }
    }

    impl T {
        fn v(&self, n: &str) -> &VolumeRoot {
            self.vols.get(n).unwrap()
        }
        fn mv(&self, src: &str, dv: &str, dst: &str, on: OnClash) -> Result<Placed, OpError> {
            let d = p(dst);
            move_entry(
                &self.db,
                self.v("A"),
                &p(src),
                self.v(dv),
                &d.parent().unwrap(),
                d.name().unwrap(),
                on,
                "user:t",
                1,
            )
        }
        fn cp(
            &self,
            src: &str,
            dv: &str,
            dst: &str,
            on: OnClash,
            hidden: bool,
        ) -> Result<Placed, OpError> {
            let d = p(dst);
            copy_entry(
                &self.db,
                self.v("A"),
                &p(src),
                self.v(dv),
                &d.parent().unwrap(),
                d.name().unwrap(),
                CopySpec {
                    include_hidden: hidden,
                    shallow: false,
                },
                on,
                "user:t",
                1,
            )
        }
        fn read(&self, rel: &str) -> String {
            std::fs::read_to_string(self.root.join(rel)).unwrap()
        }
        fn uploads_empty(&self, vol: &str) {
            let dir = self.root.join(vol).join(UPLOADS_DIR);
            if dir.exists() {
                assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0, "{dir:?}");
            }
            assert!(self.db.temps().unwrap().is_empty());
        }
    }

    #[test]
    fn rename_and_clash_rules() {
        let t = setup();
        assert_eq!(
            t.mv("Angebot.pdf", "A", "Offer.pdf", OnClash::Fail)
                .unwrap(),
            done("Offer.pdf")
        );
        std::fs::write(t.root.join("a/Angebot.pdf"), "v2").unwrap();
        assert!(matches!(
            t.mv("Offer.pdf", "A", "Angebot.pdf", OnClash::Fail),
            Err(OpError::Exists(e)) if e.size == 2
        ));
        assert_eq!(
            t.mv("Offer.pdf", "A", "Angebot.pdf", OnClash::Skip)
                .unwrap(),
            Placed::Skipped
        );
        assert_eq!(
            t.mv("Offer.pdf", "A", "Angebot.pdf", OnClash::KeepBoth)
                .unwrap(),
            done("Angebot (1).pdf")
        );
        assert_eq!(t.read("a/Angebot.pdf"), "v2");
        assert!(matches!(
            t.mv("Angebot (1).pdf", "A", "Angebot.pdf", OnClash::ReplaceEmpty),
            Err(OpError::Exists(_))
        ));
        assert_eq!(
            t.mv("Angebot (1).pdf", "A", "Angebot.pdf", OnClash::Overwrite)
                .unwrap(),
            Placed::Done {
                name: "Angebot.pdf".into(),
                replaced: Replaced::InPlace
            }
        );
        assert_eq!(t.read("a/Angebot.pdf"), "v1");
        assert!(t.db.trash_items().unwrap().is_empty(), "overwrite in place");
    }

    #[test]
    fn overwrite_sends_folders_in_the_way_to_the_trash() {
        let t = setup();
        std::fs::create_dir(t.root.join("a/Ziel")).unwrap();
        std::fs::write(t.root.join("a/Ziel/keep.txt"), "k").unwrap();
        match t
            .mv("Angebot.pdf", "A", "Ziel", OnClash::Overwrite)
            .unwrap()
        {
            Placed::Done {
                replaced: Replaced::Trashed(item),
                ..
            } => assert!(item.is_dir && item.path == "Ziel"),
            other => panic!("{other:?}"),
        }
        assert_eq!(t.read("a/Ziel"), "v1");
        let item = &t.db.trash_items().unwrap()[0];
        assert_eq!(item.reason, trash::REASON_REPLACED);
        // Trash keeps the old folder.
        let trashed = t
            .root
            .join("a/.chestcraft-trash")
            .join(&item.id)
            .join("Ziel/keep.txt");
        assert!(trashed.is_file());
    }

    #[test]
    fn refused_moves() {
        let t = setup();
        assert!(matches!(
            t.mv("Fotos", "A", "Fotos/2025/Fotos", OnClash::Fail),
            Err(OpError::Invalid(_))
        ));
        assert!(matches!(
            t.mv("Fotos", "A", "Fotos", OnClash::Fail),
            Err(OpError::Invalid(_))
        ));
        assert!(matches!(
            t.mv("immich", "A", "x", OnClash::Fail),
            Err(OpError::ReadOnlyInside)
        ));
        assert!(matches!(
            t.mv("missing", "A", "x", OnClash::Fail),
            Err(OpError::NotFound)
        ));
        assert!(matches!(
            t.mv("Fotos", "A", "nope/x", OnClash::Fail),
            Err(OpError::NotFound)
        ));
        assert!(matches!(
            move_entry(
                &t.db,
                t.v("A"),
                &RelPath::root(),
                t.v("B"),
                &RelPath::root(),
                "x",
                OnClash::Fail,
                "u",
                1
            ),
            Err(OpError::Invalid(_))
        ));
    }

    #[test]
    fn moves_to_another_filesystem_copy_and_trash_the_source() {
        // /dev/shm is a separate tmpfs on most Linux systems; skip where it isn't.
        let Ok(other) = tempfile::tempdir_in("/dev/shm") else {
            return;
        };
        let t = setup();
        let dev = |p: &Path| std::fs::metadata(p).unwrap().dev();
        if dev(other.path()) == dev(&t.root) {
            return;
        }
        std::fs::create_dir(other.path().join("c")).unwrap();
        let cfg = Config::from_toml_str(&format!(
            "[server]\npublic_url = \"http://x.org\"\n[volumes.C]\npath = \"{}\"\n",
            other.path().join("c").display()
        ))
        .unwrap();
        let cvols = Volumes::open(&cfg);
        let c = cvols.get("C").unwrap();
        let placed = move_entry(
            &t.db,
            t.v("A"),
            &p("Fotos"),
            c,
            &RelPath::root(),
            "Fotos",
            OnClash::Fail,
            "user:t",
            1,
        )
        .unwrap();
        assert_eq!(placed, done("Fotos"));
        assert!(other.path().join("c/Fotos/2025/x.jpg").is_file());
        assert!(!t.root.join("a/Fotos").exists());
        let items = t.db.trash_items().unwrap();
        assert_eq!(
            (items.len(), items[0].reason.as_str()),
            (1, trash::REASON_MOVED)
        );
        assert!(t.db.temps().unwrap().is_empty());
    }

    #[test]
    fn mtimes_can_be_set() {
        let t = setup();
        set_mtime(t.v("A"), &p("Angebot.pdf"), 1_000_000_000_000).unwrap();
        assert_eq!(
            std::fs::metadata(t.root.join("a/Angebot.pdf"))
                .unwrap()
                .mtime(),
            1_000_000_000
        );
        set_mtime(t.v("A"), &RelPath::root(), 2_000_000_000_000).unwrap();
        assert_eq!(
            std::fs::metadata(t.root.join("a")).unwrap().mtime(),
            2_000_000_000
        );
        assert!(matches!(
            set_mtime(t.v("A"), &p("nope"), 0),
            Err(OpError::NotFound)
        ));
    }

    #[test]
    fn mount_points_are_found_in_mountinfo() {
        let info = "36 35 98:0 /mnt1 /data/media rw - zfs tank/media rw\n\
                    37 35 98:0 / /data/media/video\\040clips rw - zfs tank/clips rw\n";
        assert!(mounts_at_or_below(info, Path::new("/data/media")));
        assert!(mounts_at_or_below(
            info,
            Path::new("/data/media/video clips")
        ));
        assert!(!mounts_at_or_below(info, Path::new("/data/media/photos")));
        assert!(!mounts_at_or_below(info, Path::new("/data/media/video")));
        assert_eq!(unescape_mount_path("a\\040b\\134c\\04"), "a b\\c\\04");
    }

    #[test]
    fn copy_keeps_times_modes_and_links_and_skips_hidden_unless_asked() {
        let t = setup();
        assert_eq!(
            t.cp("Fotos", "B", "Fotos", OnClash::Fail, false).unwrap(),
            done("Fotos")
        );
        let x = t.root.join("b/Fotos/2025/x.jpg");
        assert_eq!(std::fs::read(&x).unwrap(), vec![7u8; 100]);
        assert_eq!(std::fs::metadata(&x).unwrap().mtime(), 1_600_000_000);
        assert_eq!(std::fs::metadata(&x).unwrap().mode() & 0o777, 0o644);
        assert_eq!(
            std::fs::metadata(t.root.join("b/Fotos/run.sh"))
                .unwrap()
                .mode()
                & 0o777,
            0o755
        );
        assert_eq!(
            std::fs::metadata(t.root.join("b/Fotos")).unwrap().mode() & 0o777,
            0o755
        );
        assert_eq!(
            std::fs::read_link(t.root.join("b/Fotos/link")).unwrap(),
            Path::new("2025/x.jpg")
        );
        t.uploads_empty("b");
        // Source untouched.
        assert!(t.root.join("a/Fotos/2025/x.jpg").is_file());

        // The volume root: no dotfiles unless asked, never internal folders.
        std::fs::create_dir_all(t.root.join("a/.chestcraft-trash/zz")).unwrap();
        std::fs::create_dir(t.root.join("b/all")).unwrap();
        let d = p("all");
        copy_entry(
            &t.db,
            t.v("A"),
            &RelPath::root(),
            t.v("B"),
            &d,
            "plain",
            CopySpec::default(),
            OnClash::Fail,
            "u",
            1,
        )
        .unwrap();
        copy_entry(
            &t.db,
            t.v("A"),
            &RelPath::root(),
            t.v("B"),
            &d,
            "hidden",
            CopySpec {
                include_hidden: true,
                shallow: false,
            },
            OnClash::Fail,
            "u",
            1,
        )
        .unwrap();
        copy_entry(
            &t.db,
            t.v("A"),
            &p("Fotos"),
            t.v("B"),
            &d,
            "shallow",
            CopySpec {
                include_hidden: false,
                shallow: true,
            },
            OnClash::Fail,
            "u",
            1,
        )
        .unwrap();
        assert!(!t.root.join("b/all/plain/.git").exists());
        assert!(t.root.join("b/all/hidden/.git/HEAD").is_file());
        assert!(!t.root.join("b/all/hidden/.chestcraft-trash").exists());
        assert_eq!(
            std::fs::read_dir(t.root.join("b/all/shallow"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn copy_clashes_trash_or_keep_both() {
        let t = setup();
        assert_eq!(
            t.cp("Angebot.pdf", "B", "Angebot.pdf", OnClash::KeepBoth, false)
                .unwrap(),
            done("Angebot (1).pdf")
        );
        match t
            .cp("Angebot.pdf", "B", "Angebot.pdf", OnClash::Trash, false)
            .unwrap()
        {
            Placed::Done {
                replaced: Replaced::Trashed(item),
                ..
            } => assert_eq!(item.volume, "B"),
            other => panic!("{other:?}"),
        }
        assert_eq!(t.read("b/Angebot.pdf"), "v1");
        assert!(matches!(
            t.cp("Angebot.pdf", "B", "Angebot.pdf", OnClash::Fail, false),
            Err(OpError::Exists(_))
        ));
        assert_eq!(
            t.cp("Angebot.pdf", "B", "Angebot.pdf", OnClash::Skip, false)
                .unwrap(),
            Placed::Skipped
        );
        // An empty file may be replaced without the right to delete.
        assert_eq!(
            t.cp(
                "Angebot.pdf",
                "B",
                "empty.txt",
                OnClash::ReplaceEmpty,
                false
            )
            .unwrap(),
            Placed::Done {
                name: "empty.txt".into(),
                replaced: Replaced::InPlace
            }
        );
        t.uploads_empty("b");
    }

    #[test]
    fn new_files_appear_complete_and_never_overwrite_by_accident() {
        let t = setup();
        let vol = t.vols.get("B").unwrap().clone();
        let mut f = NewFile::create(&t.db, &vol, &RelPath::root(), "neu.txt", Some(5), 1).unwrap();
        f.write_all(b"hel").unwrap();
        f.write_all(b"lo").unwrap();
        assert!(!t.root.join("b/neu.txt").exists());
        assert_eq!(t.db.temps().unwrap().len(), 1);
        assert_eq!(
            f.commit(Some(1_500_000_000_123), OnClash::Fail, "u", 1)
                .unwrap(),
            done("neu.txt")
        );
        let m = std::fs::metadata(t.root.join("b/neu.txt")).unwrap();
        assert_eq!(
            (m.mtime(), m.mtime_nsec(), m.mode() & 0o777),
            (1_500_000_000, 123_000_000, 0o644)
        );
        t.uploads_empty("b");

        let f = NewFile::create(&t.db, &vol, &RelPath::root(), "Angebot.pdf", None, 1).unwrap();
        assert!(matches!(
            f.commit(None, OnClash::ReplaceEmpty, "u", 1),
            Err(OpError::Exists(_))
        ));
        assert_eq!(t.read("b/Angebot.pdf"), "old");
        t.uploads_empty("b");

        let mut f = NewFile::create(&t.db, &vol, &RelPath::root(), "Angebot.pdf", None, 1).unwrap();
        f.write_all(b"new").unwrap();
        drop(f);
        t.uploads_empty("b");
        assert!(matches!(
            NewFile::create(&t.db, &vol, &p("missing"), "x", None, 1),
            Err(OpError::NotFound)
        ));
        assert!(matches!(
            NewFile::create(&t.db, &vol, &RelPath::root(), "x", Some(u64::MAX), 1),
            Err(OpError::NoSpace)
        ));
        t.uploads_empty("b");
    }

    #[test]
    fn sweep_removes_leftovers_of_a_crash() {
        let t = setup();
        let vol = t.vols.get("B").unwrap().clone();
        let mut f = NewFile::create(&t.db, &vol, &RelPath::root(), "x", None, 100).unwrap();
        f.write_all(b"partial").unwrap();
        // A crash: neither the file nor the record are cleaned up.
        std::mem::forget(f);
        assert_eq!(sweep_temps(&t.db, &t.vols, Some(1000), 500), 0, "too young");
        assert_eq!(sweep_temps(&t.db, &t.vols, None, 500), 1);
        t.uploads_empty("b");
    }
}
