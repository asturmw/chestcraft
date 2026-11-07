//! Nightly copy of chestcraft's own state to `paths.backup_dir`.
//!
//! The state directory sits on the system disk, away from the pool it reports on; the copy
//! goes to `backup_dir` (meant to be a dataset on that pool, so its snapshots keep the
//! history). Layout:
//!
//! ```text
//! <backup_dir>/state.redb               the database (consistent copy, mode 600)
//! <backup_dir>/config/config.toml       the config file (no secrets in it)
//! <backup_dir>/config/i18n/<code>.toml  extra languages, if any
//! <backup_dir>/backgrounds/<set id>/…   background pictures, mirrored
//! ```
//!
//! Every file is written under a temporary name, synced and renamed into place, so a crash
//! leaves the previous copy intact. Only the database, the config and the backgrounds are
//! copied: secrets stay where they are, thumbnails are a cache. `backup_dir` itself must
//! exist and lie on another file system than `state_dir`: a copy on the same disk isn't a
//! backup, and a dataset that isn't mounted leaves an empty folder on the system disk
//! that would quietly take the copy.

use crate::db::Db;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

pub const DB_FILE: &str = "state.redb";
pub const CONFIG_DIR: &str = "config";
pub const BACKGROUNDS_DIR: &str = "backgrounds";

#[derive(Debug, Default, PartialEq)]
pub struct Report {
    /// Database rows copied.
    pub rows: u64,
    /// Size of the database copy.
    pub bytes: u64,
    pub config_files: usize,
    /// Background files copied this time (unchanged ones are skipped).
    pub pictures_copied: usize,
    /// Background files or sets dropped from the copy because they are gone from the state.
    pub pictures_dropped: usize,
}

/// Both paths exist and are on the same file system.
pub fn same_filesystem(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (fs::metadata(a), fs::metadata(b)) {
        (Ok(x), Ok(y)) => x.dev() == y.dev(),
        _ => false,
    }
}

/// The reason not to back up into `backup_dir`, if any.
pub fn refuse(state_dir: &Path, backup_dir: &Path) -> Option<String> {
    if !backup_dir.is_dir() {
        return Some(format!("{} doesn't exist", backup_dir.display()));
    }
    if same_filesystem(state_dir, backup_dir) {
        return Some(format!(
            "{} is on the same file system as the state ({}): is its dataset mounted?",
            backup_dir.display(),
            state_dir.display()
        ));
    }
    None
}

/// Copy the state. `config_file` is the file the server was started with. The caller
/// checks [`refuse`] first.
pub fn run(
    db: &Db,
    state_dir: &Path,
    backup_dir: &Path,
    config_file: Option<&Path>,
) -> Result<Report, String> {
    if !backup_dir.is_dir() {
        return Err(format!("{} doesn't exist", backup_dir.display()));
    }
    let mut r = Report::default();

    let tmp = backup_dir.join(format!(".{DB_FILE}.tmp"));
    let dest = backup_dir.join(DB_FILE);
    remove_leftover(&tmp).map_err(|e| err("removing", &tmp, e))?;
    r.rows = db
        .copy_to(&tmp)
        .map_err(|e| format!("{}: {e}", tmp.display()))?;
    r.bytes = finish(&tmp, &dest).map_err(|e| err("writing", &dest, e))?;

    if let Some(file) = config_file {
        let dir = backup_dir.join(CONFIG_DIR);
        make_dir(&dir).map_err(|e| err("creating", &dir, e))?;
        copy_file(file, &dir.join("config.toml")).map_err(|e| err("copying", file, e))?;
        r.config_files += 1;
        if let Some(i18n) = file.parent().map(|d| d.join("i18n"))
            && i18n.is_dir()
        {
            let out = dir.join("i18n");
            make_dir(&out).map_err(|e| err("creating", &out, e))?;
            for e in fs::read_dir(&i18n).map_err(|e| err("reading", &i18n, e))? {
                let e = e.map_err(|e| err("reading", &i18n, e))?;
                let name = e.file_name();
                if e.file_type().is_ok_and(|t| t.is_file())
                    && Path::new(&name).extension().is_some_and(|x| x == "toml")
                {
                    copy_file(&e.path(), &out.join(&name))
                        .map_err(|err2| err("copying", &e.path(), err2))?;
                    r.config_files += 1;
                }
            }
        }
    }

    let (copied, dropped) = mirror(
        &state_dir.join(BACKGROUNDS_DIR),
        &backup_dir.join(BACKGROUNDS_DIR),
    )?;
    r.pictures_copied = copied;
    r.pictures_dropped = dropped;
    Ok(r)
}

fn err(what: &str, path: &Path, e: io::Error) -> String {
    format!("{what} {}: {e}", path.display())
}

/// A temporary file of an earlier, interrupted run.
fn remove_leftover(tmp: &Path) -> io::Result<()> {
    match fs::remove_file(tmp) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

fn make_dir(dir: &Path) -> io::Result<()> {
    match fs::create_dir(dir) {
        Err(e) if e.kind() != io::ErrorKind::AlreadyExists => Err(e),
        _ => fs::set_permissions(dir, fs::Permissions::from_mode(0o700)),
    }
}

/// Make `tmp` private, sync it and rename it to `dest`; returns its size.
fn finish(tmp: &Path, dest: &Path) -> io::Result<u64> {
    fs::set_permissions(tmp, fs::Permissions::from_mode(0o600))?;
    let f = fs::File::open(tmp)?;
    f.sync_all()?;
    let size = f.metadata()?.len();
    drop(f);
    fs::rename(tmp, dest)?;
    if let Some(dir) = dest.parent() {
        fs::File::open(dir)?.sync_all()?;
    }
    Ok(size)
}

fn copy_file(src: &Path, dest: &Path) -> io::Result<u64> {
    let name = dest.file_name().unwrap_or_default().to_string_lossy();
    let tmp = dest.with_file_name(format!(".{name}.tmp"));
    remove_leftover(&tmp)?;
    fs::copy(src, &tmp)?;
    finish(&tmp, dest)
}

/// Make `dest` hold the same set folders and files as `src`. Files are written once and
/// never change, so a file of the same size is taken as copied already.
fn mirror(src: &Path, dest: &Path) -> Result<(usize, usize), String> {
    let (mut copied, mut dropped) = (0, 0);
    let sets: Vec<fs::DirEntry> = match fs::read_dir(src) {
        Ok(entries) => entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .collect(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(err("reading", src, e)),
    };
    if sets.is_empty() && !dest.exists() {
        return Ok((0, 0));
    }
    make_dir(dest).map_err(|e| err("creating", dest, e))?;
    let mut keep = std::collections::HashSet::new();
    for set in &sets {
        let name = set.file_name();
        keep.insert(name.clone());
        let out = dest.join(&name);
        make_dir(&out).map_err(|e| err("creating", &out, e))?;
        let mut files = std::collections::HashSet::new();
        for f in fs::read_dir(set.path()).map_err(|e| err("reading", &set.path(), e))? {
            let f = f.map_err(|e| err("reading", &set.path(), e))?;
            let Ok(meta) = f.metadata() else { continue };
            if !meta.is_file() || f.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            files.insert(f.file_name());
            let target = out.join(f.file_name());
            if fs::metadata(&target).is_ok_and(|m| m.is_file() && m.len() == meta.len()) {
                continue;
            }
            copy_file(&f.path(), &target).map_err(|e| err("copying", &f.path(), e))?;
            copied += 1;
        }
        for f in fs::read_dir(&out)
            .map_err(|e| err("reading", &out, e))?
            .flatten()
        {
            if !files.contains(&f.file_name()) && f.file_type().is_ok_and(|t| t.is_file()) {
                fs::remove_file(f.path()).map_err(|e| err("removing", &f.path(), e))?;
                dropped += 1;
            }
        }
    }
    for e in fs::read_dir(dest)
        .map_err(|e| err("reading", dest, e))?
        .flatten()
    {
        if !keep.contains(&e.file_name()) && e.file_type().is_ok_and(|t| t.is_dir()) {
            fs::remove_dir_all(e.path()).map_err(|err2| err("removing", &e.path(), err2))?;
            dropped += 1;
        }
    }
    Ok((copied, dropped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{FILE_NAME, User};

    fn user(name: &str) -> User {
        User {
            name: name.into(),
            role: "admin".into(),
            password_hash: "x".into(),
            must_change_password: false,
            disabled: false,
            created: 1,
            last_login: None,
            settings: Default::default(),
            known_remote_ips: Vec::new(),
            kind: Default::default(),
        }
    }

    #[test]
    fn copies_database_config_and_pictures() {
        let t = tempfile::tempdir().unwrap();
        let (state, backup, etc) = (
            t.path().join("state"),
            t.path().join("b"),
            t.path().join("etc"),
        );
        for d in [&state, &backup, &etc.join("i18n")] {
            fs::create_dir_all(d).unwrap();
        }
        let db = Db::open(&state.join(FILE_NAME)).unwrap();
        db.put_user(&user("Jana")).unwrap();
        fs::write(etc.join("config.toml"), "[server]\n").unwrap();
        fs::write(etc.join("i18n/fr.toml"), "x = 1\n").unwrap();
        fs::write(etc.join("i18n/notes.txt"), "no").unwrap();
        let bg = state.join("backgrounds");
        fs::create_dir_all(bg.join("set1")).unwrap();
        fs::write(bg.join("set1/wide.jpg"), b"jpeg").unwrap();
        fs::write(bg.join("set1/.wide.jpg.part"), b"half").unwrap();

        let r = run(&db, &state, &backup, Some(&etc.join("config.toml"))).unwrap();
        assert!(r.rows > 1 && r.bytes > 0, "{r:?}");
        assert_eq!(r.config_files, 2);
        assert_eq!((r.pictures_copied, r.pictures_dropped), (1, 0));
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&backup.join(DB_FILE)), 0o600);
        assert_eq!(mode(&backup.join("config/config.toml")), 0o600);
        assert!(backup.join("config/i18n/fr.toml").is_file());
        assert!(!backup.join("config/i18n/notes.txt").exists());
        assert!(!backup.join("backgrounds/set1/.wide.jpg.part").exists());
        assert!(!backup.join(".state.redb.tmp").exists());

        // The copy opens as a database with the same content and tables.
        db.put_user(&user("Later")).unwrap();
        drop(db);
        let copy = Db::open(&backup.join(DB_FILE)).unwrap();
        let names: Vec<String> = copy.users().unwrap().into_iter().map(|u| u.name).collect();
        assert_eq!(names, ["Jana"]);
        drop(copy);
        let tables = |p: &Path| {
            use redb::{ReadableDatabase, TableHandle};
            let d = redb::Database::open(p).unwrap();
            let mut v: Vec<String> = d
                .begin_read()
                .unwrap()
                .list_tables()
                .unwrap()
                .map(|t| t.name().to_string())
                .collect();
            v.sort();
            v
        };
        assert_eq!(
            tables(&state.join(FILE_NAME)),
            tables(&backup.join(DB_FILE))
        );

        // Second run: unchanged pictures are skipped, a removed set leaves the copy.
        let db = Db::open(&state.join(FILE_NAME)).unwrap();
        fs::create_dir_all(bg.join("set2")).unwrap();
        fs::write(bg.join("set2/tall.jpg"), b"jpeg2").unwrap();
        fs::remove_dir_all(bg.join("set1")).unwrap();
        let r = run(&db, &state, &backup, None).unwrap();
        assert_eq!((r.pictures_copied, r.pictures_dropped), (1, 1));
        assert!(!backup.join("backgrounds/set1").exists());
        assert!(backup.join("backgrounds/set2/tall.jpg").is_file());
    }

    #[test]
    fn missing_backup_dir_is_an_error() {
        let t = tempfile::tempdir().unwrap();
        let db = Db::open(&t.path().join(FILE_NAME)).unwrap();
        let e = run(&db, t.path(), &t.path().join("absent"), None).unwrap_err();
        assert!(e.contains("doesn't exist"), "{e}");
        assert!(!t.path().join("absent").exists());
    }

    #[test]
    fn same_filesystem_is_refused() {
        let t = tempfile::tempdir().unwrap();
        let (a, b) = (t.path().join("a"), t.path().join("b"));
        fs::create_dir(&a).unwrap();
        assert!(refuse(&a, &b).unwrap().contains("doesn't exist"));
        fs::create_dir(&b).unwrap();
        assert!(refuse(&a, &b).unwrap().contains("same file system"));
        // /proc is always its own file system.
        assert!(!same_filesystem(t.path(), Path::new("/proc")));
        assert!(refuse(&a, Path::new("/proc")).is_none());
    }

    #[test]
    fn leftover_temp_file_is_replaced() {
        let t = tempfile::tempdir().unwrap();
        let state = t.path().join("s");
        fs::create_dir(&state).unwrap();
        let db = Db::open(&state.join(FILE_NAME)).unwrap();
        fs::write(t.path().join(".state.redb.tmp"), b"junk").unwrap();
        run(&db, &state, t.path(), None).unwrap();
        assert!(!t.path().join(".state.redb.tmp").exists());
        drop(db);
        Db::open(&t.path().join(DB_FILE)).unwrap();
    }
}
