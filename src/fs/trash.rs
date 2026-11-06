//! The trash: deleting moves an item to `<fs root>/.chestcraft-trash/<id>/<name>` (a rename
//! on the same filesystem, instant even for large folders) and records it in the database.
//! Restoring moves it back and never overwrites. Purging (after `trash_days`, or when the
//! owner empties a volume's trash) is the only place chestcraft deletes user files for good.
//!
//! Order of steps: the database record is written before the move and removed after the
//! item is gone, so a crash leaves at worst a record without files, which purging drops.

use crate::config::Config;
use crate::db::{AuditEvent, Db, TrashItem};
use crate::fs::names::Variant;
use crate::fs::ops::{self, EntryKind, OpError, TRASH_DIR};
use crate::fs::path::RelPath;
use crate::fs::volume::{VolumeRoot, Volumes};
use rustix::fs::{AtFlags, Mode, mkdirat, unlinkat};
use std::io;
use std::os::fd::{AsFd, OwnedFd};

/// Why an item went to the trash.
pub const REASON_DELETE: &str = "delete";
pub const REASON_REPLACED: &str = "replaced";
pub const REASON_UNFINISHED: &str = "unfinished_upload";
/// Moved to another filesystem: the copy is in place, the original waits here.
pub const REASON_MOVED: &str = "moved";

/// Move `rel` (a file or folder of `vol`) to the trash. The caller has checked that `actor`
/// may delete it.
pub fn delete(
    db: &Db,
    vol: &VolumeRoot,
    rel: &RelPath,
    actor: &str,
    reason: &str,
    now: i64,
) -> Result<TrashItem, OpError> {
    let (Some(parent), Some(name)) = (rel.parent(), rel.name()) else {
        return Err(OpError::Invalid("the volume itself can't be deleted"));
    };
    if vol.holds_read_only(rel) {
        return Err(OpError::ReadOnlyInside);
    }
    if vol.holds_volume(rel) {
        return Err(OpError::VolumeInside);
    }
    let pfd = vol.open_dir(&parent)?;
    let parent_dev = ops::fd_dev(&pfd)?;
    let (kind, st) = ops::lstat(&pfd, name)?;
    let (is_dir, size, files) = match kind {
        EntryKind::Dir => {
            if ops::dev_of(&st) != parent_dev {
                return Err(OpError::MountPoint);
            }
            let (size, files) = ops::scan_tree(&pfd, name, parent_dev)?;
            (true, size, files)
        }
        EntryKind::Symlink => {
            // Links that leave the volume aren't listed, so they can't be deleted either.
            vol.stat(rel)?;
            (false, 0, 1)
        }
        EntryKind::File | EntryKind::Other => (false, ops::existing(name, kind, &st).size, 1),
    };
    let fs_root = ops::fs_root(vol, &parent)?;
    let tdir = ops::work_dir(vol, &fs_root, TRASH_DIR)?;
    let item = TrashItem {
        id: ops::new_id(),
        volume: vol.name().to_string(),
        path: rel.as_str().to_string(),
        fs_root: fs_root.as_str().to_string(),
        is_dir,
        size,
        files,
        deleted_at: now,
        deleted_by: actor.to_string(),
        reason: reason.to_string(),
    };
    put(db, &tdir, &pfd, name, &item)?;
    Ok(item)
}

/// Record `item`, then move `src_name` from `src_dir` into the trash folder `tdir` (both on
/// the same filesystem).
pub fn put(
    db: &Db,
    tdir: &OwnedFd,
    src_dir: impl AsFd,
    src_name: &str,
    item: &TrashItem,
) -> Result<(), OpError> {
    let name = RelPath::parse(&item.path)?
        .name()
        .ok_or(OpError::Invalid("path"))?
        .to_string();
    db.put_trash(item)?;
    let moved = (|| -> io::Result<()> {
        mkdirat(tdir, item.id.as_str(), Mode::from_raw_mode(0o700))?;
        let idfd = ops::open_subdir(tdir, &item.id)?;
        ops::rename_noreplace(src_dir, src_name, &idfd, &name)
    })();
    if let Err(e) = moved {
        let _ = unlinkat(tdir, item.id.as_str(), AtFlags::REMOVEDIR);
        let _ = db.delete_trash(&item.id);
        return Err(e.into());
    }
    Ok(())
}

/// The trash folder an item lives in.
fn trash_dir_of(vol: &VolumeRoot, item: &TrashItem) -> Result<OwnedFd, OpError> {
    let root = RelPath::parse(&item.fs_root)?;
    Ok(vol.open_dir(&root.join(TRASH_DIR)?)?)
}

/// Move an item back to where it was, recreating missing folders. If the name is taken,
/// it comes back as "name (restored)", and shares of it follow. Returns the path it got.
pub fn restore(db: &Db, vol: &VolumeRoot, item: &TrashItem) -> Result<RelPath, OpError> {
    let rel = RelPath::parse(&item.path)?;
    let (Some(parent), Some(name)) = (rel.parent(), rel.name()) else {
        return Err(OpError::Invalid("path"));
    };
    let tdir = trash_dir_of(vol, item).map_err(gone)?;
    let idfd = ops::open_subdir(&tdir, &item.id).map_err(|e| gone(e.into()))?;
    let pfd = ops::mkdirs(vol, &parent)?;
    let used = ops::place_unique(&idfd, name, &pfd, name, item.is_dir, Variant::Restored)
        .map_err(|e| gone(e.into()))?;
    let _ = unlinkat(&tdir, item.id.as_str(), AtFlags::REMOVEDIR);
    db.delete_trash(&item.id)?;
    let restored = parent.join(&used)?;
    if restored != rel
        && let Err(e) = crate::shares::follow_move(db, vol.name(), &rel, vol.name(), &restored)
    {
        tracing::error!(volume = %vol.name(), path = %rel, "shares didn't follow a restore: {e}");
    }
    Ok(restored)
}

fn gone(e: OpError) -> OpError {
    match e {
        OpError::NotFound => OpError::Gone,
        other => other,
    }
}

/// Delete an item for good and drop its record. A missing item only loses its record.
pub fn purge(db: &Db, vol: &VolumeRoot, item: &TrashItem) -> Result<(), OpError> {
    match trash_dir_of(vol, item) {
        Ok(tdir) => {
            let dev = ops::fd_dev(&tdir)?;
            match ops::remove_tree(&tdir, &item.id, dev) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Err(OpError::NotFound) => {}
        Err(e) => return Err(e),
    }
    db.delete_trash(&item.id)?;
    Ok(())
}

/// When `item` is due for purging (Unix seconds), or None if its volume keeps the trash.
pub fn purge_time(cfg: &Config, item: &TrashItem) -> Option<i64> {
    let days = cfg.volumes.get(&item.volume)?.trash_days(cfg);
    (days > 0).then(|| item.deleted_at + i64::from(days) * 86_400)
}

/// Purge `items` and audit each one. Returns (purged, failed).
pub fn purge_items(
    db: &Db,
    volumes: &Volumes,
    items: &[TrashItem],
    actor: &str,
    now: i64,
) -> (usize, usize) {
    let (mut purged, mut failed) = (0, 0);
    for item in items {
        let Some(vol) = volumes.get(&item.volume) else {
            continue;
        };
        match purge(db, vol, item) {
            Ok(()) => {
                purged += 1;
                let _ = db.audit(&AuditEvent {
                    time: now,
                    actor: actor.to_string(),
                    ip: String::new(),
                    action: "trash_purged".into(),
                    target: format!("{}/{}", item.volume, item.path),
                    detail: format!(
                        "{} bytes, {} files, deleted by {}",
                        item.size, item.files, item.deleted_by
                    ),
                });
            }
            Err(e) => {
                failed += 1;
                tracing::error!(volume = %item.volume, path = %item.path, id = %item.id, "can't purge from the trash: {e}");
            }
        }
    }
    (purged, failed)
}

/// The daily purge: every item older than its volume's `trash_days`.
pub fn purge_due(db: &Db, volumes: &Volumes, cfg: &Config, now: i64) -> (usize, usize) {
    let items = match db.trash_items() {
        Ok(items) => items,
        Err(e) => {
            tracing::error!("can't read the trash index: {e}");
            return (0, 1);
        }
    };
    let due: Vec<TrashItem> = items
        .into_iter()
        .filter(|i| purge_time(cfg, i).is_some_and(|t| t <= now))
        .collect();
    purge_items(db, volumes, &due, "system", now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::FILE_NAME;
    use std::path::Path;

    struct T {
        _tmp: tempfile::TempDir,
        root: std::path::PathBuf,
        cfg: Config,
        vols: Volumes,
        db: Db,
    }

    fn setup(extra: &str) -> T {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("vol");
        std::fs::create_dir_all(root.join("Fotos/2025")).unwrap();
        std::fs::write(root.join("Fotos/2025/a.jpg"), vec![1u8; 300]).unwrap();
        std::fs::write(root.join("Fotos/b.jpg"), vec![2u8; 50]).unwrap();
        std::fs::write(root.join("Angebot.pdf"), "v1").unwrap();
        let cfg = Config::from_toml_str(&format!(
            "[server]\npublic_url = \"http://x.org\"\n[volumes.T]\npath = \"{}\"\n{extra}",
            root.display()
        ))
        .unwrap();
        let vols = Volumes::open(&cfg);
        let db = Db::open(&tmp.path().join(FILE_NAME)).unwrap();
        T {
            root,
            cfg,
            vols,
            db,
            _tmp: tmp,
        }
    }

    fn p(s: &str) -> RelPath {
        RelPath::parse(s).unwrap()
    }

    fn trash_entries(root: &Path) -> usize {
        std::fs::read_dir(root.join(TRASH_DIR)).unwrap().count()
    }

    #[test]
    fn delete_then_restore_round_trip() {
        let t = setup("");
        let v = t.vols.get("T").unwrap();
        let item = delete(&t.db, v, &p("Fotos"), "user:jana", REASON_DELETE, 100).unwrap();
        assert!(
            item.is_dir && item.files == 2 && item.size == 350,
            "{item:?}"
        );
        assert!(!t.root.join("Fotos").exists());
        assert!(
            t.root
                .join(TRASH_DIR)
                .join(&item.id)
                .join("Fotos/2025/a.jpg")
                .is_file()
        );
        assert_eq!(t.db.trash_items().unwrap(), vec![item.clone()]);

        let back = restore(&t.db, v, &item).unwrap();
        assert_eq!(back, p("Fotos"));
        assert!(t.root.join("Fotos/2025/a.jpg").is_file());
        assert_eq!(trash_entries(&t.root), 0);
        assert!(t.db.trash_items().unwrap().is_empty());
    }

    #[test]
    fn restore_never_overwrites_and_recreates_parents() {
        let t = setup("");
        let v = t.vols.get("T").unwrap();
        let a = delete(&t.db, v, &p("Angebot.pdf"), "user:jana", REASON_DELETE, 1).unwrap();
        std::fs::write(t.root.join("Angebot.pdf"), "v2").unwrap();
        assert_eq!(restore(&t.db, v, &a).unwrap(), p("Angebot (restored).pdf"));
        assert_eq!(std::fs::read(t.root.join("Angebot.pdf")).unwrap(), b"v2");
        assert_eq!(
            std::fs::read(t.root.join("Angebot (restored).pdf")).unwrap(),
            b"v1"
        );

        let b = delete(
            &t.db,
            v,
            &p("Fotos/2025/a.jpg"),
            "user:jana",
            REASON_DELETE,
            1,
        )
        .unwrap();
        delete(&t.db, v, &p("Fotos"), "user:jana", REASON_DELETE, 1).unwrap();
        assert_eq!(restore(&t.db, v, &b).unwrap(), p("Fotos/2025/a.jpg"));
        assert!(t.root.join("Fotos/2025/a.jpg").is_file());
    }

    #[test]
    fn read_only_areas_and_the_root_stay() {
        let t = setup("read_only_paths = [\"Fotos/2025\"]");
        let v = t.vols.get("T").unwrap();
        assert!(matches!(
            delete(&t.db, v, &p("Fotos"), "user:jana", REASON_DELETE, 1),
            Err(OpError::ReadOnlyInside)
        ));
        assert!(matches!(
            delete(&t.db, v, &RelPath::root(), "user:jana", REASON_DELETE, 1),
            Err(OpError::Invalid(_))
        ));
        assert!(matches!(
            delete(&t.db, v, &p("missing"), "user:jana", REASON_DELETE, 1),
            Err(OpError::NotFound)
        ));
        assert!(t.db.trash_items().unwrap().is_empty());
    }

    #[test]
    fn purge_respects_trash_days_and_audits() {
        let t = setup("trash_days = 2");
        let v = t.vols.get("T").unwrap();
        let day = 86_400;
        let old = delete(&t.db, v, &p("Fotos"), "user:jana", REASON_DELETE, 0).unwrap();
        let new = delete(&t.db, v, &p("Angebot.pdf"), "user:jana", REASON_DELETE, day).unwrap();
        assert_eq!(purge_time(&t.cfg, &old), Some(2 * day));
        assert_eq!(purge_due(&t.db, &t.vols, &t.cfg, 2 * day), (1, 0));
        let left = t.db.trash_items().unwrap();
        assert_eq!(left, vec![new]);
        assert_eq!(trash_entries(&t.root), 1);
        let audit = t.db.recent_audit(5).unwrap();
        assert_eq!(audit[0].action, "trash_purged");
        assert_eq!(audit[0].target, "T/Fotos");
        assert_eq!(purge_due(&t.db, &t.vols, &t.cfg, 3 * day), (1, 0));
        assert_eq!(trash_entries(&t.root), 0);
    }

    #[test]
    fn zero_days_keeps_forever_and_missing_files_only_drop_the_record() {
        let t = setup("trash_days = 0");
        let v = t.vols.get("T").unwrap();
        let item = delete(&t.db, v, &p("Angebot.pdf"), "user:jana", REASON_DELETE, 0).unwrap();
        assert_eq!(purge_time(&t.cfg, &item), None);
        assert_eq!(purge_due(&t.db, &t.vols, &t.cfg, i64::MAX / 2), (0, 0));
        std::fs::remove_dir_all(t.root.join(TRASH_DIR).join(&item.id)).unwrap();
        assert!(matches!(restore(&t.db, v, &item), Err(OpError::Gone)));
        purge(&t.db, v, &item).unwrap();
        assert!(t.db.trash_items().unwrap().is_empty());
    }
}
