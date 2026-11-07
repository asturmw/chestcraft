//! Volume roots and file access beneath them.
//!
//! Every volume keeps an `O_PATH` descriptor of its root directory. Files are opened with
//! `openat2(RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS)` relative to that descriptor, so the
//! kernel refuses any path, symlinks included, that would leave the volume. All calls here
//! block; async code runs them in `spawn_blocking`.

use crate::config::{Config, VolumeConfig};
use crate::fs::path::{RelPath, is_internal_name};
use rustix::fs::{
    AtFlags, Dir, FileType, Mode, OFlags, ResolveFlags, Stat, fstat, fstatvfs, open, openat2,
    statat,
};
use rustix::io::Errno;
use std::collections::BTreeMap;
use std::io;
use std::os::fd::OwnedFd;
use std::sync::Arc;

const BENEATH: ResolveFlags = ResolveFlags::BENEATH.union(ResolveFlags::NO_MAGICLINKS);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Dir,
    File,
    Other,
}

/// Metadata of one file or directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meta {
    pub kind: Kind,
    pub size: u64,
    /// Modification time, Unix seconds.
    pub mtime: i64,
    pub mtime_nsec: u32,
    pub ino: u64,
    pub dev: u64,
}

impl Meta {
    // rustix's `Stat` field types differ per architecture; the casts are no-ops on x86_64.
    #[allow(clippy::unnecessary_cast)]
    pub fn from_stat(st: &Stat) -> Meta {
        let kind = match FileType::from_raw_mode(st.st_mode as _) {
            FileType::Directory => Kind::Dir,
            FileType::RegularFile => Kind::File,
            _ => Kind::Other,
        };
        Meta {
            kind,
            size: st.st_size as u64,
            mtime: st.st_mtime as i64,
            mtime_nsec: st.st_mtime_nsec as u32,
            ino: st.st_ino as u64,
            dev: st.st_dev as u64,
        }
    }

    /// Strong validator for HTTP caching: same inode, size and mtime means same bytes.
    pub fn etag(&self) -> String {
        format!(
            "\"{:x}-{:x}-{:x}.{:x}\"",
            self.ino, self.size, self.mtime, self.mtime_nsec
        )
    }
}

/// One directory entry.
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub meta: Meta,
}

/// Free and total bytes of the filesystem (ZFS dataset) a volume lives on.
#[derive(Debug, Clone, Copy)]
pub struct Space {
    pub avail: u64,
    pub total: u64,
}

pub struct VolumeRoot {
    pub cfg: VolumeConfig,
    root: OwnedFd,
    /// Roots of other volumes inside this one (by configured path), relative to it.
    nested: Vec<RelPath>,
}

/// The open volumes. Volumes whose directory can't be opened are left out (and logged),
/// so one unmounted dataset doesn't take down the rest.
#[derive(Clone)]
pub struct Volumes {
    map: BTreeMap<String, Arc<VolumeRoot>>,
}

impl Volumes {
    pub fn open(cfg: &Config) -> Volumes {
        let mut map = BTreeMap::new();
        for v in cfg.volumes.values() {
            match VolumeRoot::open(v.clone()) {
                Ok(mut root) => {
                    root.nested = nested_roots(v, cfg);
                    map.insert(v.name.clone(), Arc::new(root));
                }
                Err(e) => {
                    tracing::error!(volume = %v.name, path = %v.path.display(), "can't open volume: {e}")
                }
            }
        }
        Volumes { map }
    }

    pub fn get(&self, name: &str) -> Option<&Arc<VolumeRoot>> {
        self.map.get(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<VolumeRoot>> {
        self.map.values()
    }
}

/// Other volumes whose configured path lies inside `v`'s, relative to `v`.
fn nested_roots(v: &VolumeConfig, cfg: &Config) -> Vec<RelPath> {
    cfg.volumes
        .values()
        .filter(|o| o.name != v.name)
        .filter_map(|o| o.path.strip_prefix(&v.path).ok())
        .filter_map(|rest| RelPath::parse(rest.to_str()?).ok())
        .filter(|r| !r.is_root())
        .collect()
}

/// `openat2` wants "." for the root itself.
fn arg(rel: &RelPath) -> &str {
    if rel.is_root() { "." } else { rel.as_str() }
}

/// `openat2` beneath the volume root. The kernel answers EAGAIN when a rename or mount
/// anywhere on the system races with resolving a `..` (from a symlink target); it can't
/// rule out an escape then, so the call is simply repeated.
fn open_beneath(root: &OwnedFd, rel: &RelPath, flags: OFlags) -> Result<OwnedFd, Errno> {
    let mut tries = 0;
    loop {
        match openat2(root, arg(rel), flags, Mode::empty(), BENEATH) {
            Err(Errno::AGAIN) if tries < 64 => tries += 1,
            other => return other,
        }
    }
}

/// Map errors that mean "not there for you" to NotFound, so callers answer 404 without
/// telling escapes, loops and missing files apart.
fn not_found_ish(e: Errno) -> io::Error {
    match e {
        Errno::NOENT | Errno::XDEV | Errno::LOOP | Errno::NOTDIR | Errno::ACCESS | Errno::PERM => {
            io::Error::new(io::ErrorKind::NotFound, e)
        }
        other => io::Error::from(other),
    }
}

impl VolumeRoot {
    pub fn open(cfg: VolumeConfig) -> io::Result<VolumeRoot> {
        let root = open(
            &cfg.path,
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        Ok(VolumeRoot {
            cfg,
            root,
            nested: Vec::new(),
        })
    }

    pub fn name(&self) -> &str {
        &self.cfg.name
    }

    /// Is `rel` another volume's root, or does it contain one? Such folders can't be moved
    /// or deleted: the other volume would go with them.
    pub fn holds_volume(&self, rel: &RelPath) -> bool {
        self.nested.iter().any(|n| n.starts_with(rel))
    }

    /// Is `rel` a read-only area, or does it contain one?
    pub fn holds_read_only(&self, rel: &RelPath) -> bool {
        self.cfg
            .read_only_paths
            .iter()
            .any(|ro| ro.starts_with(rel))
    }

    /// Open a file or directory for reading. Symlinks are followed only while they stay
    /// inside the volume. `O_NONBLOCK` keeps a FIFO from blocking the open; callers check
    /// the kind with [`Self::meta`] before reading.
    pub fn open_read(&self, rel: &RelPath) -> io::Result<OwnedFd> {
        open_beneath(
            &self.root,
            rel,
            OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
        )
        .map_err(not_found_ish)
    }

    pub fn meta(fd: &OwnedFd) -> io::Result<Meta> {
        Ok(Meta::from_stat(&fstat(fd)?))
    }

    /// Stat a path without opening it for reading.
    pub fn stat(&self, rel: &RelPath) -> io::Result<Meta> {
        let fd =
            open_beneath(&self.root, rel, OFlags::PATH | OFlags::CLOEXEC).map_err(not_found_ish)?;
        Self::meta(&fd)
    }

    /// Open a directory for listing or walking.
    pub fn open_dir(&self, rel: &RelPath) -> io::Result<OwnedFd> {
        open_beneath(
            &self.root,
            rel,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        )
        .map_err(not_found_ish)
    }

    /// List a directory. Internal names are never returned; dot-names only with
    /// `include_hidden`. Symlinks show their target's metadata and are left out when the
    /// target is missing or outside the volume. Names that aren't UTF-8 are skipped.
    pub fn list(&self, rel: &RelPath, include_hidden: bool) -> io::Result<Vec<Entry>> {
        let dirfd = self.open_dir(rel)?;
        let mut dir = Dir::read_from(&dirfd)?;
        let mut out = Vec::new();
        while let Some(ent) = dir.read() {
            let ent = ent?;
            let cname = ent.file_name();
            let bytes = cname.to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            let Ok(name) = std::str::from_utf8(bytes) else {
                tracing::warn!(volume = %self.name(), dir = %rel, "skipping a name that isn't UTF-8");
                continue;
            };
            if is_internal_name(name) || (!include_hidden && name.starts_with('.')) {
                continue;
            }
            let Ok(st) = statat(&dirfd, cname, AtFlags::SYMLINK_NOFOLLOW) else {
                continue;
            };
            let meta = if FileType::from_raw_mode(st.st_mode as _) == FileType::Symlink {
                let Ok(child) = rel.join(name) else { continue };
                match self.stat(&child) {
                    Ok(m) => m,
                    Err(_) => continue,
                }
            } else {
                Meta::from_stat(&st)
            };
            out.push(Entry {
                name: name.to_string(),
                meta,
            });
        }
        Ok(out)
    }

    pub fn space(&self) -> io::Result<Space> {
        let s = fstatvfs(&self.root)?;
        Ok(Space {
            avail: s.f_bavail * s.f_frsize,
            total: s.f_blocks * s.f_frsize,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn volume(dir: &std::path::Path) -> VolumeRoot {
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
    fn symlinks_cannot_leave_the_volume() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "nope").unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/inside.txt"), "ok").unwrap();
        symlink(outside.path().join("secret.txt"), root.join("abs-escape")).unwrap();
        symlink("../../etc/passwd", root.join("sub/rel-escape")).unwrap();
        symlink("sub/inside.txt", root.join("good-link")).unwrap();
        symlink(outside.path(), root.join("dir-escape")).unwrap();
        let v = volume(root);

        assert_eq!(
            v.open_read(&p("abs-escape")).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(
            v.open_read(&p("sub/rel-escape")).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(
            v.open_read(&p("dir-escape/secret.txt")).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        let fd = v.open_read(&p("good-link")).unwrap();
        assert_eq!(VolumeRoot::meta(&fd).unwrap().kind, Kind::File);

        let mut names: Vec<_> = v
            .list(&RelPath::root(), false)
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["good-link", "sub"],
            "escaping symlinks are not listed"
        );
    }

    #[test]
    fn listing_hides_dotfiles_and_internal_names() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for d in [".chestcraft-trash", ".hist", ".git", "Fotos"] {
            std::fs::create_dir(root.join(d)).unwrap();
        }
        std::fs::write(root.join(".directory"), "").unwrap();
        std::fs::write(root.join("Rechnung.pdf"), "pdf").unwrap();
        let v = volume(root);

        let mut plain: Vec<_> = v
            .list(&RelPath::root(), false)
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        plain.sort();
        assert_eq!(plain, ["Fotos", "Rechnung.pdf"]);
        let mut all: Vec<_> = v
            .list(&RelPath::root(), true)
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        all.sort();
        assert_eq!(all, [".directory", ".git", "Fotos", "Rechnung.pdf"]);
    }

    #[test]
    fn volumes_inside_volumes_are_known() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("medien/video/anime")).unwrap();
        let cfg = Config::from_toml_str(&format!(
            "[server]\npublic_url = \"http://x.org\"\n\
             [volumes.Medien]\npath = \"{0}/medien\"\n\
             [volumes.Animes]\npath = \"{0}/medien/video/anime\"\n",
            tmp.path().display()
        ))
        .unwrap();
        let vols = Volumes::open(&cfg);
        let m = vols.get("Medien").unwrap();
        assert!(m.holds_volume(&p("video")) && m.holds_volume(&p("video/anime")));
        assert!(!m.holds_volume(&p("video/anime/x")) && !m.holds_volume(&p("fotos")));
        assert!(!vols.get("Animes").unwrap().holds_volume(&RelPath::root()));
    }

    #[test]
    fn meta_and_space() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.bin"), vec![0u8; 1234]).unwrap();
        let v = volume(tmp.path());
        let m = v.stat(&p("a.bin")).unwrap();
        assert_eq!((m.kind, m.size), (Kind::File, 1234));
        assert_eq!(v.stat(&RelPath::root()).unwrap().kind, Kind::Dir);
        assert!(v.space().unwrap().total > 0);
        assert_eq!(
            v.stat(&p("missing")).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    /// systemd's RestrictSUIDSGID= blocks openat2() outright (ENOSYS), which would break
    /// every file access of the server units (found on staging, 2026-11-07).
    #[test]
    fn server_units_allow_openat2() {
        for unit in [
            include_str!("../../deploy/units/chestcraft.service"),
            include_str!("../../deploy/units/chestcraft-staging.service"),
        ] {
            assert!(
                !unit
                    .lines()
                    .any(|l| l.trim_start().starts_with("RestrictSUIDSGID=")),
                "RestrictSUIDSGID= in a server unit"
            );
        }
    }
}
