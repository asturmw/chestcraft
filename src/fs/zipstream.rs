//! Streaming zip of a directory tree: stored (no compression, media doesn't shrink), zip64
//! when a file needs it, written in one pass to any `Write`.
//!
//! The walk opens each child relative to its parent's descriptor with `O_NOFOLLOW`, so it
//! never follows symlinks (they're left out, as are devices and FIFOs). Internal names are
//! always skipped, dot-names unless `include_hidden`.

use crate::fs::path::{RelPath, is_internal_name};
use crate::fs::sort::natural_cmp;
use crate::fs::volume::{Meta, VolumeRoot};
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, fstat, openat, statat};
use std::io::{self, Write};
use std::os::fd::OwnedFd;
use zip::write::{SimpleFileOptions, StreamWriter};
use zip::{CompressionMethod, DateTime, ZipWriter};

pub struct ZipSpec {
    pub include_hidden: bool,
    /// Only these top-level names (a selection); None = everything.
    pub only: Option<Vec<String>>,
    pub tz: jiff::tz::TimeZone,
}

fn zip_err(e: zip::result::ZipError) -> io::Error {
    match e {
        zip::result::ZipError::Io(io) => io,
        other => io::Error::other(other),
    }
}

fn zip_time(unix: i64, tz: &jiff::tz::TimeZone) -> DateTime {
    jiff::Timestamp::from_second(unix)
        .ok()
        .map(|ts| ts.to_zoned(tz.clone()))
        .and_then(|z| {
            DateTime::from_date_and_time(
                z.year().clamp(1980, 2107) as u16,
                z.month() as u8,
                z.day() as u8,
                z.hour() as u8,
                z.minute() as u8,
                z.second() as u8,
            )
            .ok()
        })
        .unwrap_or_default()
}

fn options(meta: &Meta, tz: &jiff::tz::TimeZone, mode: u32) -> SimpleFileOptions {
    SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .last_modified_time(zip_time(meta.mtime, tz))
        .unix_permissions(mode)
        .large_file(meta.size >= u64::from(u32::MAX))
}

/// Write the zip of `base` (a directory in `vol`) to `out`. Returns the number of files.
pub fn write_zip<W: Write>(
    vol: &VolumeRoot,
    base: &RelPath,
    spec: &ZipSpec,
    out: W,
) -> io::Result<u64> {
    let dirfd = vol.open_dir(base)?;
    let mut zip = ZipWriter::new_stream(out);
    let mut count = 0;
    walk(&mut zip, &dirfd, "", spec, true, &mut count)?;
    let mut inner = zip.finish().map_err(zip_err)?;
    inner.flush()?;
    Ok(count)
}

/// The names a download of `dirfd` covers, in natural order: never internal names,
/// dot-names only with `include_hidden`, and only the `only` names when given (a selection
/// at the top). Shared by the zip and the tree listing (`fs::tree`).
pub fn listed_names(
    dirfd: &OwnedFd,
    include_hidden: bool,
    only: Option<&[String]>,
) -> io::Result<Vec<String>> {
    let mut names = Vec::new();
    let mut dir = Dir::read_from(dirfd)?;
    while let Some(ent) = dir.read() {
        let ent = ent?;
        let bytes = ent.file_name().to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        let Ok(name) = std::str::from_utf8(bytes) else {
            continue;
        };
        if is_internal_name(name) || (!include_hidden && name.starts_with('.')) {
            continue;
        }
        if only.is_some_and(|only| !only.iter().any(|n| n == name)) {
            continue;
        }
        names.push(name.to_string());
    }
    names.sort_by(|a, b| natural_cmp(a, b));
    Ok(names)
}

fn walk<W: Write>(
    zip: &mut ZipWriter<StreamWriter<W>>,
    dirfd: &OwnedFd,
    prefix: &str,
    spec: &ZipSpec,
    top: bool,
    count: &mut u64,
) -> io::Result<()> {
    let only = spec.only.as_deref().filter(|_| top);
    let names = listed_names(dirfd, spec.include_hidden, only)?;

    for name in names {
        let Ok(st) = statat(dirfd, name.as_str(), AtFlags::SYMLINK_NOFOLLOW) else {
            continue;
        };
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
                        tracing::warn!("zip: skipping folder {prefix}{name}: {e}");
                        continue;
                    }
                };
                let path = format!("{prefix}{name}/");
                zip.add_directory(
                    path.clone(),
                    options(&Meta::from_stat(&st), &spec.tz, 0o755),
                )
                .map_err(zip_err)?;
                walk(zip, &sub, &path, spec, false, count)?;
            }
            FileType::RegularFile => {
                let fd = match openat(
                    dirfd,
                    name.as_str(),
                    OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                    Mode::empty(),
                ) {
                    Ok(fd) => fd,
                    Err(e) => {
                        tracing::warn!("zip: skipping file {prefix}{name}: {e}");
                        continue;
                    }
                };
                let meta = Meta::from_stat(&fstat(&fd)?);
                zip.start_file(format!("{prefix}{name}"), options(&meta, &spec.tz, 0o644))
                    .map_err(zip_err)?;
                let mut file = std::fs::File::from(fd);
                io::copy(&mut file, zip)?;
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
    use std::io::{Cursor, Read};

    #[test]
    fn zips_a_tree_and_reads_back() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("Urlaub/Tag 2")).unwrap();
        std::fs::write(root.join("Urlaub/IMG_10.jpg"), b"ten").unwrap();
        std::fs::write(root.join("Urlaub/IMG_2.jpg"), b"two").unwrap();
        std::fs::write(root.join("Urlaub/Tag 2/Straße.txt"), "ß".repeat(1000)).unwrap();
        std::fs::write(root.join("Urlaub/.hidden"), b"h").unwrap();
        std::fs::create_dir(root.join("Urlaub/.chestcraft-trash")).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", root.join("Urlaub/link")).unwrap();
        let cfg = Config::from_toml_str(&format!(
            "[server]\npublic_url = \"http://x.org\"\n[volumes.T]\npath = \"{}\"\n",
            root.display()
        ))
        .unwrap();
        let vol = VolumeRoot::open(cfg.volumes["T"].clone()).unwrap();
        let spec = ZipSpec {
            include_hidden: false,
            only: None,
            tz: cfg.server.timezone.clone(),
        };

        let mut buf = Vec::new();
        let n = write_zip(&vol, &RelPath::parse("Urlaub").unwrap(), &spec, &mut buf).unwrap();
        assert_eq!(n, 3);
        let mut archive = zip::ZipArchive::new(Cursor::new(buf)).unwrap();
        let names: Vec<String> = archive
            .file_names()
            .map(|n| n.unwrap().into_owned())
            .collect();
        assert_eq!(
            names,
            ["IMG_2.jpg", "IMG_10.jpg", "Tag 2/", "Tag 2/Straße.txt"]
        );
        let mut s = String::new();
        archive
            .by_name("Tag 2/Straße.txt")
            .unwrap()
            .read_to_string(&mut s)
            .unwrap();
        assert_eq!(s, "ß".repeat(1000));

        let only = ZipSpec {
            only: Some(vec!["IMG_10.jpg".into()]),
            ..spec
        };
        let mut buf = Vec::new();
        write_zip(&vol, &RelPath::parse("Urlaub").unwrap(), &only, &mut buf).unwrap();
        let archive = zip::ZipArchive::new(Cursor::new(buf)).unwrap();
        let names: Vec<String> = archive
            .file_names()
            .map(|n| n.unwrap().into_owned())
            .collect();
        assert_eq!(names, ["IMG_10.jpg"]);
    }
}
