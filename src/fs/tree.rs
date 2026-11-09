//! Recursive listing of a folder for downloads that save file by file (the "save to
//! folder" and "separately" choices of the download menu): one JSON line per entry,
//! `{"path":"sub/a.jpg","dir":false,"size":123,"mtime":"2026-11-04T21:14:00Z"}`, folders
//! before their contents, in natural order.
//!
//! The rules are the zip's (`fs::zipstream`): the walk opens each folder relative to its
//! parent with `O_NOFOLLOW`, leaves out symlinks, devices and FIFOs, never shows internal
//! names, and dot-names only with `include_hidden`. `only` limits the top level to a
//! selection. [`usage`] adds a folder up with the same rules (folder sizes in listings).

use crate::fs::path::RelPath;
use crate::fs::volume::{Meta, VolumeRoot};
use crate::fs::zipstream::listed_names;
use rustix::fs::{AtFlags, FileType, Mode, OFlags, openat, statat};
use std::io::{self, Write};
use std::os::fd::OwnedFd;

pub struct TreeSpec {
    pub include_hidden: bool,
    /// Only these top-level names (a selection); None = everything.
    pub only: Option<Vec<String>>,
}

/// Write the listing of `base` (a folder in `vol`) to `out`. Returns the number of lines.
pub fn write_tree<W: Write>(
    vol: &VolumeRoot,
    base: &RelPath,
    spec: &TreeSpec,
    mut out: W,
) -> io::Result<u64> {
    let dirfd = vol.open_dir(base)?;
    let mut count = 0;
    walk(&mut out, &dirfd, "", spec, true, &mut count)?;
    out.flush()?;
    Ok(count)
}

fn line<W: Write>(out: &mut W, path: &str, dir: bool, meta: &Meta) -> io::Result<()> {
    let mtime = jiff::Timestamp::from_second(meta.mtime)
        .map(|t| t.to_string())
        .ok();
    let v = serde_json::json!({
        "path": path,
        "dir": dir,
        "size": if dir { 0 } else { meta.size },
        "mtime": mtime,
    });
    serde_json::to_writer(&mut *out, &v)?;
    out.write_all(b"\n")
}

fn walk<W: Write>(
    out: &mut W,
    dirfd: &OwnedFd,
    prefix: &str,
    spec: &TreeSpec,
    top: bool,
    count: &mut u64,
) -> io::Result<()> {
    let only = spec.only.as_deref().filter(|_| top);
    for name in listed_names(dirfd, spec.include_hidden, only)? {
        let Ok(st) = statat(dirfd, name.as_str(), AtFlags::SYMLINK_NOFOLLOW) else {
            continue;
        };
        let path = format!("{prefix}{name}");
        match FileType::from_raw_mode(st.st_mode as _) {
            FileType::Directory => {
                let sub = match openat(
                    dirfd,
                    name.as_str(),
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                ) {
                    Ok(fd) => fd,
                    Err(e) => {
                        tracing::warn!("tree: skipping folder {path}: {e}");
                        continue;
                    }
                };
                line(out, &path, true, &Meta::from_stat(&st))?;
                *count += 1;
                walk(out, &sub, &format!("{path}/"), spec, false, count)?;
            }
            FileType::RegularFile => {
                line(out, &path, false, &Meta::from_stat(&st))?;
                *count += 1;
            }
            _ => {}
        }
    }
    Ok(())
}

/// What a folder holds, added up by [`usage`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct Usage {
    /// Sum of the files' sizes (what a download of the folder would be).
    pub bytes: u64,
    pub files: u64,
    pub dirs: u64,
    /// False when the walk stopped at `limits` (too many entries or out of time): then the
    /// folder holds at least this much.
    pub complete: bool,
}

/// Where [`usage`] stops, so a huge folder can't keep a request busy.
pub struct UsageLimits {
    pub max_entries: u64,
    pub deadline: std::time::Instant,
}

/// Add up the files under `base`, with the tree's walk rules (no symlinks, no internal
/// names, dot-names only with `include_hidden`). Unreadable subfolders are left out.
pub fn usage(
    vol: &VolumeRoot,
    base: &RelPath,
    include_hidden: bool,
    limits: &UsageLimits,
) -> io::Result<Usage> {
    let dirfd = vol.open_dir(base)?;
    let mut u = Usage {
        complete: true,
        ..Usage::default()
    };
    add_up(&dirfd, include_hidden, limits, &mut u)?;
    Ok(u)
}

fn add_up(
    dirfd: &OwnedFd,
    include_hidden: bool,
    limits: &UsageLimits,
    u: &mut Usage,
) -> io::Result<()> {
    for name in listed_names(dirfd, include_hidden, None)? {
        if u.files + u.dirs >= limits.max_entries
            || ((u.files + u.dirs).is_multiple_of(256)
                && std::time::Instant::now() >= limits.deadline)
        {
            u.complete = false;
            return Ok(());
        }
        let Ok(st) = statat(dirfd, name.as_str(), AtFlags::SYMLINK_NOFOLLOW) else {
            continue;
        };
        match FileType::from_raw_mode(st.st_mode as _) {
            FileType::Directory => {
                u.dirs += 1;
                let Ok(sub) = openat(
                    dirfd,
                    name.as_str(),
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                ) else {
                    continue;
                };
                add_up(&sub, include_hidden, limits, u)?;
                if !u.complete {
                    return Ok(());
                }
            }
            FileType::RegularFile => {
                u.files += 1;
                u.bytes += st.st_size as u64;
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn vol(root: &std::path::Path) -> VolumeRoot {
        let cfg = Config::from_toml_str(&format!(
            "[server]\npublic_url = \"http://x.org\"\n[volumes.T]\npath = \"{}\"\n",
            root.display()
        ))
        .unwrap();
        VolumeRoot::open(cfg.volumes["T"].clone()).unwrap()
    }

    fn lines(v: &VolumeRoot, spec: &TreeSpec) -> Vec<serde_json::Value> {
        let mut out = Vec::new();
        write_tree(v, &RelPath::default(), spec, &mut out).unwrap();
        String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn lists_the_tree_like_the_zip() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("Urlaub/Tag 10")).unwrap();
        std::fs::create_dir_all(root.join("Urlaub/Tag 2")).unwrap();
        std::fs::create_dir_all(root.join("leer")).unwrap();
        std::fs::create_dir_all(root.join(".chestcraft-trash/x")).unwrap();
        std::fs::write(root.join("Urlaub/Tag 2/a.jpg"), b"abc").unwrap();
        std::fs::write(root.join("Urlaub/.hidden"), b"h").unwrap();
        std::fs::write(root.join("b.txt"), b"hello").unwrap();
        std::os::unix::fs::symlink("/etc/passwd", root.join("Urlaub/link")).unwrap();
        let mtime = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_793_826_840);
        std::fs::File::options()
            .write(true)
            .open(root.join("b.txt"))
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        let v = vol(root);

        let all = lines(
            &v,
            &TreeSpec {
                include_hidden: false,
                only: None,
            },
        );
        let paths: Vec<&str> = all.iter().map(|l| l["path"].as_str().unwrap()).collect();
        assert_eq!(
            paths,
            [
                "b.txt",
                "leer",
                "Urlaub",
                "Urlaub/Tag 2",
                "Urlaub/Tag 2/a.jpg",
                "Urlaub/Tag 10"
            ]
        );
        assert_eq!(
            all[0],
            serde_json::json!({"path": "b.txt", "dir": false, "size": 5,
                "mtime": "2026-11-04T21:14:00Z"})
        );
        assert_eq!(
            (all[1]["dir"].as_bool(), all[1]["size"].as_u64()),
            (Some(true), Some(0))
        );

        let hidden = lines(
            &v,
            &TreeSpec {
                include_hidden: true,
                only: Some(vec!["Urlaub".into()]),
            },
        );
        let paths: Vec<&str> = hidden.iter().map(|l| l["path"].as_str().unwrap()).collect();
        assert_eq!(
            paths,
            [
                "Urlaub",
                "Urlaub/.hidden",
                "Urlaub/Tag 2",
                "Urlaub/Tag 2/a.jpg",
                "Urlaub/Tag 10"
            ]
        );
    }

    #[test]
    fn usage_adds_up_like_the_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::create_dir_all(root.join(".chestcraft-trash")).unwrap();
        std::fs::write(root.join("a/one"), vec![0u8; 100]).unwrap();
        std::fs::write(root.join("a/b/two"), vec![0u8; 20]).unwrap();
        std::fs::write(root.join("a/.dot"), vec![0u8; 3]).unwrap();
        std::fs::write(root.join(".chestcraft-trash/gone"), vec![0u8; 1000]).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", root.join("a/link")).unwrap();
        let v = vol(root);
        let far = UsageLimits {
            max_entries: 1000,
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(60),
        };
        let u = usage(&v, &RelPath::default(), false, &far).unwrap();
        assert_eq!(
            u,
            Usage {
                bytes: 120,
                files: 2,
                dirs: 2,
                complete: true
            }
        );
        let u = usage(&v, &RelPath::default(), true, &far).unwrap();
        assert_eq!((u.bytes, u.files), (123, 3));
        let a = RelPath::parse("a").unwrap();
        assert_eq!(usage(&v, &a, false, &far).unwrap().bytes, 120);
        let tight = UsageLimits {
            max_entries: 2,
            ..far
        };
        let u = usage(&v, &RelPath::default(), false, &tight).unwrap();
        assert!(!u.complete);
        assert!(u.files + u.dirs <= 2);
    }
}
