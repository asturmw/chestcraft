//! Recursive listing of a folder for downloads that save file by file (the "save to
//! folder" and "separately" choices of the download menu): one JSON line per entry,
//! `{"path":"sub/a.jpg","dir":false,"size":123,"mtime":"2026-11-04T21:14:00Z"}`, folders
//! before their contents, in natural order.
//!
//! The rules are the zip's (`fs::zipstream`): the walk opens each folder relative to its
//! parent with `O_NOFOLLOW`, leaves out symlinks, devices and FIFOs, never shows internal
//! names, and dot-names only with `include_hidden`. `only` limits the top level to a
//! selection.

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
}
