//! Chunked, resumable uploads.
//!
//! The client announces a file (target folder, relative name, size, modification time),
//! sends fixed-size chunks in any order and in parallel, then finishes it. Data goes to
//! `<fs root>/.chestcraft-uploads/<id>.part` on the target's filesystem, so finishing is an
//! atomic rename that never overwrites: a taken name is resolved by the client's choice
//! (keep both, replace = the old file goes to the trash, skip).
//!
//! The database keeps every unfinished upload and the chunks known to be on disk. Chunks are
//! recorded only after an fdatasync (every [`SYNC_BYTES`]), so after a crash the client
//! resends at most that much; while the server runs, status answers from memory.
//!
//! Batches group a folder upload. On volumes with `stage_folder_uploads`, files below a
//! folder are finished into `<id>.batch/` next to the `.part` files, and the batch's
//! folders move into the target together when the client finishes the batch.
//!
//! Every call blocks; async code runs them in `spawn_blocking`.

use crate::db::{BatchRecord, Db, TrashItem, UploadRecord};
use crate::fs::names::Variant;
use crate::fs::ops::{self, EntryKind, Existing, OpError, TRASH_DIR, UPLOADS_DIR};
use crate::fs::path::RelPath;
use crate::fs::trash;
use crate::fs::volume::{VolumeRoot, Volumes};
use rustix::fs::{
    AtFlags, Mode, OFlags, Timespec, Timestamps, UTIME_NOW, fchmod, fdatasync, fstatvfs, fsync,
    ftruncate, futimens, mkdirat, openat, unlinkat,
};
use rustix::io::Errno;
use serde::Serialize;
use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex, MutexGuard};

/// Unsynced bytes that trigger an fdatasync and a progress record.
pub const SYNC_BYTES: u64 = 256 << 20;

/// What to do when the target name is taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clash {
    /// Report the clash; the client asks the user.
    Ask,
    /// Keep both: `name (1).ext`.
    Rename,
    /// The old file goes to the trash (files only, never folders).
    Replace,
    Skip,
}

impl Clash {
    pub fn parse(s: &str) -> Option<Clash> {
        Some(match s {
            "ask" => Clash::Ask,
            "rename" => Clash::Rename,
            "replace" => Clash::Replace,
            "skip" => Clash::Skip,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Clash::Ask => "ask",
            Clash::Rename => "rename",
            Clash::Replace => "replace",
            Clash::Skip => "skip",
        }
    }
}

pub struct NewUpload<'a> {
    pub owner: &'a str,
    /// Target folder.
    pub dir: &'a RelPath,
    /// Path below `dir`, every component checked with `validate_new_name`.
    pub name: &'a RelPath,
    pub size: u64,
    pub mtime_ms: i64,
    pub clash: Clash,
    pub batch: Option<&'a BatchRecord>,
    pub chunk: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Status {
    pub id: String,
    pub size: u64,
    pub chunk: u64,
    pub chunks: u32,
    /// Chunk indices received.
    pub have: Vec<u32>,
}

#[derive(Debug)]
pub enum Created {
    Started(Status),
    Skipped,
    Clash(Existing),
}

#[derive(Debug)]
pub enum Finished {
    /// `path` is relative to the target folder; `renamed` when the name was taken.
    Done {
        path: RelPath,
        renamed: bool,
    },
    Skipped,
    Clash(Existing),
}

struct SlotState {
    rec: UploadRecord,
    file: Arc<File>,
    have: Vec<bool>,
    unsynced: u64,
    writers: u32,
    /// Being finished or cancelled: no more chunks.
    closing: bool,
}

struct Slot {
    st: Mutex<SlotState>,
}

impl Slot {
    fn lock(&self) -> MutexGuard<'_, SlotState> {
        self.st.lock().unwrap_or_else(|p| p.into_inner())
    }
}

pub struct Uploads {
    db: Arc<Db>,
    slots: Mutex<HashMap<String, Arc<Slot>>>,
}

fn chunk_count(size: u64, chunk: u64) -> u32 {
    u32::try_from(size.div_ceil(chunk)).unwrap_or(u32::MAX)
}

fn to_ranges(have: &[bool]) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = Vec::new();
    for (i, h) in have.iter().enumerate() {
        if !h {
            continue;
        }
        let i = i as u32;
        match out.last_mut() {
            Some((_, end)) if *end == i => *end = i + 1,
            _ => out.push((i, i + 1)),
        }
    }
    out
}

fn from_ranges(ranges: &[(u32, u32)], chunks: u32) -> Vec<bool> {
    let mut have = vec![false; chunks as usize];
    for &(a, b) in ranges {
        for i in a..b.min(chunks) {
            have[i as usize] = true;
        }
    }
    have
}

fn part_name(id: &str) -> String {
    format!("{id}.part")
}

fn batch_dir_name(id: &str) -> String {
    format!("{id}.batch")
}

fn status_of(st: &SlotState) -> Status {
    Status {
        id: st.rec.id.clone(),
        size: st.rec.size,
        chunk: st.rec.chunk,
        chunks: st.have.len() as u32,
        have: st
            .have
            .iter()
            .enumerate()
            .filter(|(_, h)| **h)
            .map(|(i, _)| i as u32)
            .collect(),
    }
}

fn timestamps(mtime_ms: i64) -> Timestamps {
    Timestamps {
        last_access: Timespec {
            tv_sec: 0,
            tv_nsec: UTIME_NOW,
        },
        last_modification: Timespec {
            tv_sec: mtime_ms.div_euclid(1000),
            tv_nsec: mtime_ms.rem_euclid(1000) * 1_000_000,
        },
    }
}

fn gone(e: OpError) -> OpError {
    match e {
        OpError::NotFound => OpError::Gone,
        other => other,
    }
}

/// The uploads folder of a record's filesystem root.
fn uploads_dir(vol: &VolumeRoot, fs_root: &str) -> Result<OwnedFd, OpError> {
    let root = RelPath::parse(fs_root)?;
    Ok(vol.open_dir(&root.join(UPLOADS_DIR)?)?)
}

/// Remove an upload's `.part` file (an abandoned or cancelled upload).
fn remove_part(vol: &VolumeRoot, rec: &UploadRecord) -> Result<(), OpError> {
    let updir = match uploads_dir(vol, &rec.fs_root) {
        Ok(d) => d,
        Err(OpError::NotFound) => return Ok(()),
        Err(e) => return Err(e),
    };
    match unlinkat(&updir, part_name(&rec.id).as_str(), AtFlags::empty()) {
        Ok(()) | Err(Errno::NOENT) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// A claim on one chunk. Dropping it without [`ChunkTicket::complete`] (an error, or the
/// client went away) releases the claim; the chunk then counts as missing.
pub struct ChunkTicket {
    slot: Arc<Slot>,
    db: Arc<Db>,
    pub file: Arc<File>,
    pub offset: u64,
    pub len: u64,
    n: u32,
    completed: bool,
}

impl ChunkTicket {
    /// Write part of the chunk, `pos` bytes into it.
    pub fn write_at(&self, data: &[u8], pos: u64) -> io::Result<()> {
        use std::os::unix::fs::FileExt;
        self.file.write_all_at(data, self.offset + pos)
    }

    /// Mark the chunk as received; returns how many chunks have arrived. Syncs and records
    /// progress every [`SYNC_BYTES`].
    pub fn complete(mut self, now: i64) -> Result<u32, OpError> {
        self.completed = true;
        let (count, snapshot) = {
            let mut st = self.slot.lock();
            st.writers -= 1;
            st.have[self.n as usize] = true;
            st.unsynced += self.len;
            st.rec.touched = now;
            let count = st.have.iter().filter(|h| **h).count() as u32;
            let snapshot = (st.unsynced >= SYNC_BYTES && !st.closing).then(|| {
                st.unsynced = 0;
                UploadRecord {
                    have: to_ranges(&st.have),
                    ..st.rec.clone()
                }
            });
            (count, snapshot)
        };
        if let Some(rec) = snapshot {
            // Everything in the snapshot was written before this sync.
            fdatasync(&*self.file)?;
            self.db.put_upload(&rec, false)?;
        }
        Ok(count)
    }
}

impl Drop for ChunkTicket {
    fn drop(&mut self) {
        if !self.completed {
            self.slot.lock().writers -= 1;
        }
    }
}

impl Uploads {
    pub fn new(db: Arc<Db>) -> Uploads {
        Uploads {
            db,
            slots: Mutex::new(HashMap::new()),
        }
    }

    fn slots(&self) -> MutexGuard<'_, HashMap<String, Arc<Slot>>> {
        self.slots.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The in-memory state of an upload, loaded from the database after a restart. Someone
    /// else's upload is reported as not found.
    fn slot(&self, vols: &Volumes, id: &str, owner: &str) -> Result<Arc<Slot>, OpError> {
        let cached = self.slots().get(id).cloned();
        if let Some(slot) = cached {
            if slot.lock().rec.owner != owner {
                return Err(OpError::NotFound);
            }
            return Ok(slot);
        }
        let rec = self
            .db
            .upload(id)?
            .filter(|r| r.owner == owner)
            .ok_or(OpError::NotFound)?;
        let vol = vols.get(&rec.volume).ok_or(OpError::NotFound)?;
        let opened = uploads_dir(vol, &rec.fs_root).and_then(|d| {
            Ok(openat(
                &d,
                part_name(id).as_str(),
                OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )?)
        });
        let fd = match opened {
            Ok(fd) => fd,
            Err(OpError::NotFound) => {
                let _ = self.db.delete_upload(id);
                return Err(OpError::Gone);
            }
            Err(e) => return Err(e),
        };
        let chunks = chunk_count(rec.size, rec.chunk);
        let slot = Arc::new(Slot {
            st: Mutex::new(SlotState {
                have: from_ranges(&rec.have, chunks),
                rec,
                file: Arc::new(File::from(fd)),
                unsynced: 0,
                writers: 0,
                closing: false,
            }),
        });
        Ok(self.slots().entry(id.to_string()).or_insert(slot).clone())
    }

    /// The stored record of an upload (for permission checks before finishing).
    pub fn record(&self, vols: &Volumes, id: &str, owner: &str) -> Result<UploadRecord, OpError> {
        Ok(self.slot(vols, id, owner)?.lock().rec.clone())
    }

    pub fn status(&self, vols: &Volumes, id: &str, owner: &str) -> Result<Status, OpError> {
        Ok(status_of(&self.slot(vols, id, owner)?.lock()))
    }

    /// Start an upload: checks the name, creates missing folders and the `.part` file.
    pub fn create(&self, vol: &VolumeRoot, req: &NewUpload, now: i64) -> Result<Created, OpError> {
        let fname = req.name.name().ok_or(OpError::Invalid("name"))?;
        let sub = req.name.parent().unwrap_or_default();
        // The target folder must exist; folders below it are created as needed.
        vol.open_dir(req.dir)?;
        let staged = req.batch.is_some_and(|b| b.staged) && req.name.depth() > 1;
        let fs_root = if staged {
            RelPath::parse(&req.batch.expect("staged has a batch").fs_root)?
        } else {
            let parent = req.dir.join_path(&sub)?;
            if let Ok(pfd) = vol.open_dir(&parent) {
                match ops::lstat(&pfd, fname) {
                    Ok((kind, st)) => {
                        let ex = ops::existing(fname, kind, &st);
                        match req.clash {
                            Clash::Ask => return Ok(Created::Clash(ex)),
                            Clash::Skip => return Ok(Created::Skipped),
                            Clash::Replace if kind != EntryKind::File => {
                                return Ok(Created::Clash(ex));
                            }
                            Clash::Rename | Clash::Replace => {}
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
            ops::mkdirs(vol, &parent)?;
            ops::fs_root(vol, &parent)?
        };
        let updir = ops::work_dir(vol, &fs_root, UPLOADS_DIR)?;
        let space = fstatvfs(&updir)?;
        if req.size > space.f_bavail.saturating_mul(space.f_frsize) {
            return Err(OpError::NoSpace);
        }

        let id = ops::new_id();
        let rec = UploadRecord {
            id: id.clone(),
            owner: req.owner.to_string(),
            volume: vol.name().to_string(),
            dir: req.dir.as_str().to_string(),
            name: req.name.as_str().to_string(),
            size: req.size,
            mtime_ms: req.mtime_ms,
            chunk: req.chunk,
            clash: req.clash.as_str().to_string(),
            fs_root: fs_root.as_str().to_string(),
            batch: req.batch.map(|b| b.id.clone()),
            staged,
            created: now,
            touched: now,
            have: Vec::new(),
        };
        self.db.put_upload(&rec, true)?;
        let part = part_name(&id);
        let created = (|| -> io::Result<File> {
            let fd = openat(
                &updir,
                part.as_str(),
                OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o600),
            )?;
            ftruncate(&fd, req.size)?;
            Ok(File::from(fd))
        })();
        let file = match created {
            Ok(f) => f,
            Err(e) => {
                let _ = unlinkat(&updir, part.as_str(), AtFlags::empty());
                let _ = self.db.delete_upload(&id);
                return Err(e.into());
            }
        };
        if let Some(b) = req.batch {
            self.touch_batch(&b.id, now);
        }
        let chunks = chunk_count(req.size, req.chunk);
        let state = SlotState {
            rec,
            file: Arc::new(file),
            have: vec![false; chunks as usize],
            unsynced: 0,
            writers: 0,
            closing: false,
        };
        let status = status_of(&state);
        self.slots().insert(
            id,
            Arc::new(Slot {
                st: Mutex::new(state),
            }),
        );
        Ok(Created::Started(status))
    }

    /// Claim chunk `n` for writing. `len` is the request's Content-Length, if sent.
    pub fn begin_chunk(
        &self,
        vols: &Volumes,
        id: &str,
        owner: &str,
        n: u32,
        len: Option<u64>,
    ) -> Result<ChunkTicket, OpError> {
        let slot = self.slot(vols, id, owner)?;
        let mut st = slot.lock();
        if st.closing {
            return Err(OpError::Busy);
        }
        if n as usize >= st.have.len() {
            return Err(OpError::Invalid("chunk"));
        }
        let offset = u64::from(n) * st.rec.chunk;
        let expected = st.rec.chunk.min(st.rec.size - offset);
        if len.is_some_and(|l| l != expected) {
            return Err(OpError::Invalid("chunk length"));
        }
        st.writers += 1;
        let file = st.file.clone();
        drop(st);
        Ok(ChunkTicket {
            slot,
            db: self.db.clone(),
            file,
            offset,
            len: expected,
            n,
            completed: false,
        })
    }

    /// Finish an upload whose chunks have all arrived. `clash` overrides the choice made at
    /// the start; `can_replace` says whether the owner may delete the existing file.
    pub fn finish(
        &self,
        vols: &Volumes,
        id: &str,
        owner: &str,
        clash: Option<Clash>,
        can_replace: bool,
        now: i64,
    ) -> Result<Finished, OpError> {
        let slot = self.slot(vols, id, owner)?;
        let (rec, file) = {
            let mut st = slot.lock();
            if st.closing || st.writers > 0 {
                return Err(OpError::Busy);
            }
            if !st.have.iter().all(|h| *h) {
                return Err(OpError::Incomplete);
            }
            st.closing = true;
            (st.rec.clone(), st.file.clone())
        };
        let clash = clash
            .or_else(|| Clash::parse(&rec.clash))
            .unwrap_or(Clash::Ask);
        let result = self.finish_inner(vols, &rec, &file, clash, can_replace, now);
        match &result {
            Ok(Finished::Clash(_)) | Err(_) => slot.lock().closing = false,
            Ok(_) => {
                self.slots().remove(id);
                if let Some(b) = &rec.batch {
                    self.touch_batch(b, now);
                }
            }
        }
        result
    }

    fn finish_inner(
        &self,
        vols: &Volumes,
        rec: &UploadRecord,
        file: &File,
        clash: Clash,
        can_replace: bool,
        now: i64,
    ) -> Result<Finished, OpError> {
        let vol = vols.get(&rec.volume).ok_or(OpError::NotFound)?;
        let dir = RelPath::parse(&rec.dir)?;
        let name = RelPath::parse(&rec.name)?;
        let fname = name.name().ok_or(OpError::Invalid("name"))?;
        let sub = name.parent().unwrap_or_default();
        let updir = uploads_dir(vol, &rec.fs_root).map_err(gone)?;
        let part = part_name(&rec.id);

        // Data and metadata first: the file is complete the moment it appears.
        fdatasync(file)?;
        fchmod(file, Mode::from_raw_mode(0o644))?;
        futimens(file, &timestamps(rec.mtime_ms))?;

        let (dest, used) = if rec.staged {
            let batch = rec.batch.as_deref().ok_or(OpError::Invalid("batch"))?;
            let bdir =
                ops::open_subdir(&updir, &batch_dir_name(batch)).map_err(|e| gone(e.into()))?;
            let dest = ops::mkdirs_at(&bdir, &sub)?;
            let used = ops::place_unique(&updir, &part, &dest, fname, false, Variant::Numbered)
                .map_err(|e| gone(e.into()))?;
            (dest, used)
        } else {
            let parent = dir.join_path(&sub)?;
            let dest = ops::mkdirs(vol, &parent)?;
            match ops::rename_noreplace(&updir, &part, &dest, fname) {
                Ok(()) => (dest, fname.to_string()),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    let (kind, st) = ops::lstat(&dest, fname)?;
                    let ex = ops::existing(fname, kind, &st);
                    match clash {
                        Clash::Ask => return Ok(Finished::Clash(ex)),
                        Clash::Skip => {
                            unlinkat(&updir, part.as_str(), AtFlags::empty())?;
                            self.db.delete_upload(&rec.id)?;
                            return Ok(Finished::Skipped);
                        }
                        Clash::Rename => {
                            let used = ops::place_unique(
                                &updir,
                                &part,
                                &dest,
                                fname,
                                false,
                                Variant::Numbered,
                            )?;
                            (dest, used)
                        }
                        Clash::Replace => {
                            if kind != EntryKind::File || !can_replace {
                                return Ok(Finished::Clash(ex));
                            }
                            let target = parent.join(fname)?;
                            let used =
                                self.replace(vol, rec, &updir, &part, &dest, &target, now)?;
                            (dest, used)
                        }
                    }
                }
                Err(e) => return Err(gone(e.into())),
            }
        };
        let _ = fsync(&dest);
        self.db.delete_upload(&rec.id)?;
        Ok(Finished::Done {
            renamed: used != fname,
            path: sub.join(&used)?,
        })
    }

    /// Put the finished upload in place of `target`, whose old file goes to the trash.
    /// Returns the name used.
    #[allow(clippy::too_many_arguments)]
    fn replace(
        &self,
        vol: &VolumeRoot,
        rec: &UploadRecord,
        updir: &OwnedFd,
        part: &str,
        dest: &OwnedFd,
        target: &RelPath,
        now: i64,
    ) -> Result<String, OpError> {
        let fname = target.name().ok_or(OpError::Invalid("name"))?;
        let fs_root = RelPath::parse(&rec.fs_root)?;
        let tdir = ops::work_dir(vol, &fs_root, TRASH_DIR)?;
        match ops::exchange(updir, part, dest, fname) {
            Ok(()) => {
                // Readers never saw a missing file; the old one is now at `part`.
                let (kind, st) = ops::lstat(updir, part)?;
                let old = ops::existing(fname, kind, &st);
                let item = TrashItem {
                    id: ops::new_id(),
                    volume: vol.name().to_string(),
                    path: target.as_str().to_string(),
                    fs_root: rec.fs_root.clone(),
                    is_dir: old.is_dir,
                    size: old.size,
                    files: 1,
                    deleted_at: now,
                    deleted_by: rec.owner.clone(),
                    reason: trash::REASON_REPLACED.into(),
                };
                if let Err(e) = trash::put(&self.db, &tdir, updir, part, &item) {
                    // Swap back rather than leave the old file where cleanup would remove it.
                    let _ = ops::exchange(updir, part, dest, fname);
                    return Err(e);
                }
                Ok(fname.to_string())
            }
            Err(e) if ops::errno(&e) == Some(Errno::INVAL) => {
                // No RENAME_EXCHANGE on this filesystem: trash first, then move in.
                trash::delete(
                    &self.db,
                    vol,
                    target,
                    &rec.owner,
                    trash::REASON_REPLACED,
                    now,
                )?;
                Ok(ops::place_unique(
                    updir,
                    part,
                    dest,
                    fname,
                    false,
                    Variant::Numbered,
                )?)
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Stop an upload and remove its data.
    pub fn cancel(&self, vols: &Volumes, id: &str, owner: &str) -> Result<(), OpError> {
        let slot = self.slot(vols, id, owner)?;
        let rec = {
            let mut st = slot.lock();
            if st.closing {
                return Err(OpError::Busy);
            }
            st.closing = true;
            st.rec.clone()
        };
        let vol = vols.get(&rec.volume).ok_or(OpError::NotFound)?;
        remove_part(vol, &rec)?;
        self.db.delete_upload(id)?;
        self.slots().remove(id);
        Ok(())
    }

    // --- batches ---------------------------------------------------------------------

    pub fn create_batch(
        &self,
        vol: &VolumeRoot,
        dir: &RelPath,
        owner: &str,
        now: i64,
    ) -> Result<BatchRecord, OpError> {
        vol.open_dir(dir)?;
        let fs_root = ops::fs_root(vol, dir)?;
        let rec = BatchRecord {
            id: ops::new_id(),
            owner: owner.to_string(),
            volume: vol.name().to_string(),
            dir: dir.as_str().to_string(),
            fs_root: fs_root.as_str().to_string(),
            staged: vol.cfg.stage_folder_uploads,
            created: now,
            touched: now,
        };
        self.db.put_batch(&rec)?;
        if rec.staged {
            let made = ops::work_dir(vol, &fs_root, UPLOADS_DIR).and_then(|updir| {
                Ok(mkdirat(
                    &updir,
                    batch_dir_name(&rec.id).as_str(),
                    Mode::from_raw_mode(0o755),
                )?)
            });
            if let Err(e) = made {
                let _ = self.db.delete_batch(&rec.id);
                return Err(e);
            }
        }
        Ok(rec)
    }

    pub fn batch(&self, id: &str, owner: &str) -> Result<BatchRecord, OpError> {
        self.db
            .batch(id)?
            .filter(|b| b.owner == owner)
            .ok_or(OpError::NotFound)
    }

    fn touch_batch(&self, id: &str, now: i64) {
        if let Ok(Some(mut b)) = self.db.batch(id) {
            b.touched = now;
            let _ = self.db.put_batch(&b);
        }
    }

    /// Finish a batch once none of its uploads is open. Staged folders move into the
    /// target; returns `(staged name, name used)` for each.
    pub fn finish_batch(
        &self,
        vols: &Volumes,
        id: &str,
        owner: &str,
    ) -> Result<Vec<(String, String)>, OpError> {
        let b = self.batch(id, owner)?;
        if self
            .db
            .uploads()?
            .iter()
            .any(|u| u.batch.as_deref() == Some(id))
        {
            return Err(OpError::Incomplete);
        }
        let moved = if b.staged {
            let vol = vols.get(&b.volume).ok_or(OpError::NotFound)?;
            commit_staged(vol, &b)?
        } else {
            Vec::new()
        };
        self.db.delete_batch(id)?;
        Ok(moved)
    }

    // --- cleanup -----------------------------------------------------------------------

    /// Remove uploads untouched for `max_age` seconds, and move the files of staged batches
    /// abandoned that long to the trash (restorable until it's purged). Returns
    /// (uploads removed, batches closed).
    pub fn sweep(&self, vols: &Volumes, max_age: i64, now: i64) -> (usize, usize) {
        let cutoff = now - max_age;
        let mut removed = 0;
        for rec in self.db.uploads().unwrap_or_default() {
            let slot = self.slots().get(&rec.id).cloned();
            if let Some(slot) = &slot {
                let mut st = slot.lock();
                if st.rec.touched >= cutoff || st.writers > 0 || st.closing {
                    continue;
                }
                st.closing = true;
            } else if rec.touched >= cutoff {
                continue;
            }
            let Some(vol) = vols.get(&rec.volume) else {
                continue;
            };
            match remove_part(vol, &rec) {
                Ok(()) => {
                    let _ = self.db.delete_upload(&rec.id);
                    self.slots().remove(&rec.id);
                    removed += 1;
                }
                Err(e) => {
                    if let Some(slot) = slot {
                        slot.lock().closing = false;
                    }
                    tracing::warn!(volume = %rec.volume, id = %rec.id, "can't remove an abandoned upload: {e}");
                }
            }
        }

        let open: Vec<String> = self
            .db
            .uploads()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|u| u.batch)
            .collect();
        let mut closed = 0;
        for b in self.db.batches().unwrap_or_default() {
            if b.touched >= cutoff || open.contains(&b.id) {
                continue;
            }
            let Some(vol) = vols.get(&b.volume) else {
                continue;
            };
            let result = if b.staged {
                trash_staged(&self.db, vol, &b, now)
            } else {
                Ok(())
            };
            match result.and_then(|()| Ok(self.db.delete_batch(&b.id)?)) {
                Ok(()) => closed += 1,
                Err(e) => {
                    tracing::warn!(volume = %b.volume, id = %b.id, "can't close an abandoned folder upload: {e}")
                }
            }
        }
        (removed, closed)
    }
}

fn open_staging(vol: &VolumeRoot, b: &BatchRecord) -> Result<Option<(OwnedFd, OwnedFd)>, OpError> {
    let updir = match uploads_dir(vol, &b.fs_root) {
        Ok(d) => d,
        Err(OpError::NotFound) => return Ok(None),
        Err(e) => return Err(e),
    };
    match ops::open_subdir(&updir, &batch_dir_name(&b.id)) {
        Ok(bdir) => Ok(Some((updir, bdir))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Move the staged top-level entries of a batch into its target folder, never overwriting.
fn commit_staged(vol: &VolumeRoot, b: &BatchRecord) -> Result<Vec<(String, String)>, OpError> {
    let Some((updir, bdir)) = open_staging(vol, b)? else {
        return Ok(Vec::new());
    };
    let dest = ops::mkdirs(vol, &RelPath::parse(&b.dir)?)?;
    let mut moved = Vec::new();
    for name in ops::list_names(&bdir)? {
        let (kind, _) = ops::lstat(&bdir, &name)?;
        let used = ops::place_unique(
            &bdir,
            &name,
            &dest,
            &name,
            kind == EntryKind::Dir,
            Variant::Numbered,
        )?;
        moved.push((name, used));
    }
    unlinkat(&updir, batch_dir_name(&b.id).as_str(), AtFlags::REMOVEDIR)?;
    let _ = fsync(&dest);
    Ok(moved)
}

/// An abandoned staged batch: what arrived goes to the trash, as if it had been uploaded
/// and deleted, so its owner can still restore it.
fn trash_staged(db: &Db, vol: &VolumeRoot, b: &BatchRecord, now: i64) -> Result<(), OpError> {
    let Some((updir, bdir)) = open_staging(vol, b)? else {
        return Ok(());
    };
    let dir = RelPath::parse(&b.dir)?;
    let fs_root = RelPath::parse(&b.fs_root)?;
    let tdir = ops::work_dir(vol, &fs_root, TRASH_DIR)?;
    let dev = ops::fd_dev(&bdir)?;
    for name in ops::list_names(&bdir)? {
        let (kind, _) = ops::lstat(&bdir, &name)?;
        let (size, files) = ops::scan_tree(&bdir, &name, dev)?;
        let item = TrashItem {
            id: ops::new_id(),
            volume: b.volume.clone(),
            path: dir.join(&name)?.as_str().to_string(),
            fs_root: b.fs_root.clone(),
            is_dir: kind == EntryKind::Dir,
            size,
            files,
            deleted_at: now,
            deleted_by: b.owner.clone(),
            reason: trash::REASON_UNFINISHED.into(),
        };
        trash::put(db, &tdir, &bdir, &name, &item)?;
    }
    unlinkat(&updir, batch_dir_name(&b.id).as_str(), AtFlags::REMOVEDIR)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::db::FILE_NAME;
    use std::path::PathBuf;

    const CHUNK: u64 = 4;
    const OWNER: &str = "user:jana";

    struct T {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        vols: Volumes,
        db: Arc<Db>,
        up: Uploads,
    }

    fn setup(extra: &str) -> T {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("vol");
        std::fs::create_dir_all(root.join("Eingang")).unwrap();
        std::fs::write(root.join("Eingang/Angebot.pdf"), "old").unwrap();
        let cfg = Config::from_toml_str(&format!(
            "[server]\npublic_url = \"http://x.org\"\n[volumes.T]\npath = \"{}\"\n{extra}",
            root.display()
        ))
        .unwrap();
        let vols = Volumes::open(&cfg);
        let db = Arc::new(Db::open(&tmp.path().join(FILE_NAME)).unwrap());
        T {
            up: Uploads::new(db.clone()),
            root,
            vols,
            db,
            _tmp: tmp,
        }
    }

    fn p(s: &str) -> RelPath {
        RelPath::parse(s).unwrap()
    }

    impl T {
        fn vol(&self) -> &VolumeRoot {
            self.vols.get("T").unwrap()
        }

        fn start(
            &self,
            name: &str,
            data: &[u8],
            clash: Clash,
            batch: Option<&BatchRecord>,
        ) -> Created {
            let (dir, name) = (p("Eingang"), p(name));
            self.up
                .create(
                    self.vol(),
                    &NewUpload {
                        owner: OWNER,
                        dir: &dir,
                        name: &name,
                        size: data.len() as u64,
                        mtime_ms: 1_700_000_000_123,
                        clash,
                        batch,
                        chunk: CHUNK,
                    },
                    10,
                )
                .unwrap()
        }

        fn send_all(&self, id: &str, data: &[u8]) {
            // Out of order, like parallel chunks.
            let chunks = chunk_count(data.len() as u64, CHUNK);
            for n in (0..chunks).rev() {
                let t = self.up.begin_chunk(&self.vols, id, OWNER, n, None).unwrap();
                let (a, b) = (t.offset as usize, (t.offset + t.len) as usize);
                t.write_at(&data[a..b], 0).unwrap();
                t.complete(11).unwrap();
            }
        }

        fn upload(&self, name: &str, data: &[u8], clash: Clash) -> Finished {
            let Created::Started(s) = self.start(name, data, clash, None) else {
                panic!("not started")
            };
            self.send_all(&s.id, data);
            self.up
                .finish(&self.vols, &s.id, OWNER, None, true, 12)
                .unwrap()
        }

        fn parts(&self) -> usize {
            std::fs::read_dir(self.root.join(UPLOADS_DIR))
                .map(|d| d.count())
                .unwrap_or(0)
        }
    }

    #[test]
    fn upload_in_chunks_keeps_mtime_and_mode() {
        let t = setup("");
        let data = b"hello chunked world";
        let Finished::Done { path, renamed } = t.upload("sub/dir/greeting.txt", data, Clash::Ask)
        else {
            panic!()
        };
        assert_eq!((path.as_str(), renamed), ("sub/dir/greeting.txt", false));
        let f = t.root.join("Eingang/sub/dir/greeting.txt");
        assert_eq!(std::fs::read(&f).unwrap(), data);
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let m = std::fs::metadata(&f).unwrap();
        assert_eq!(m.permissions().mode() & 0o777, 0o644);
        assert_eq!((m.mtime(), m.mtime_nsec()), (1_700_000_000, 123_000_000));
        assert_eq!(t.parts(), 0);
        assert!(t.db.uploads().unwrap().is_empty());
    }

    #[test]
    fn finishing_needs_every_chunk_and_no_writer() {
        let t = setup("");
        let Created::Started(s) = t.start("x.bin", b"0123456789", Clash::Ask, None) else {
            panic!()
        };
        assert_eq!(s.chunks, 3);
        let ticket = t.up.begin_chunk(&t.vols, &s.id, OWNER, 2, Some(2)).unwrap();
        assert!(matches!(
            t.up.finish(&t.vols, &s.id, OWNER, None, true, 1),
            Err(OpError::Busy)
        ));
        drop(ticket);
        assert!(matches!(
            t.up.finish(&t.vols, &s.id, OWNER, None, true, 1),
            Err(OpError::Incomplete)
        ));
        assert!(matches!(
            t.up.begin_chunk(&t.vols, &s.id, OWNER, 0, Some(3)),
            Err(OpError::Invalid(_))
        ));
        assert!(matches!(
            t.up.begin_chunk(&t.vols, &s.id, OWNER, 3, None),
            Err(OpError::Invalid(_))
        ));
        assert!(matches!(
            t.up.status(&t.vols, &s.id, "user:other"),
            Err(OpError::NotFound)
        ));
    }

    #[test]
    fn name_clashes() {
        let t = setup("");
        let Created::Clash(ex) = t.start("Angebot.pdf", b"new", Clash::Ask, None) else {
            panic!()
        };
        assert_eq!(
            (ex.name.as_str(), ex.size, ex.is_dir),
            ("Angebot.pdf", 3, false)
        );
        assert!(matches!(
            t.start("Angebot.pdf", b"new", Clash::Skip, None),
            Created::Skipped
        ));

        let Finished::Done { path, renamed } = t.upload("Angebot.pdf", b"new", Clash::Rename)
        else {
            panic!()
        };
        assert_eq!((path.as_str(), renamed), ("Angebot (1).pdf", true));

        let Finished::Done { renamed, .. } = t.upload("Angebot.pdf", b"newest", Clash::Replace)
        else {
            panic!()
        };
        assert!(!renamed);
        assert_eq!(
            std::fs::read(t.root.join("Eingang/Angebot.pdf")).unwrap(),
            b"newest"
        );
        let trashed = t.db.trash_items().unwrap();
        assert_eq!(trashed.len(), 1);
        assert_eq!(
            (
                trashed[0].path.as_str(),
                trashed[0].reason.as_str(),
                trashed[0].size
            ),
            ("Eingang/Angebot.pdf", "replaced", 3)
        );
        let old = t
            .root
            .join(TRASH_DIR)
            .join(&trashed[0].id)
            .join("Angebot.pdf");
        assert_eq!(std::fs::read(old).unwrap(), b"old");
        // A folder is never replaced.
        std::fs::create_dir(t.root.join("Eingang/Ordner")).unwrap();
        assert!(matches!(
            t.start("Ordner", b"x", Clash::Replace, None),
            Created::Clash(e) if e.is_dir
        ));
    }

    #[test]
    fn a_clash_that_appears_while_uploading_is_asked_at_finish() {
        let t = setup("");
        let Created::Started(s) = t.start("neu.txt", b"abc", Clash::Ask, None) else {
            panic!()
        };
        t.send_all(&s.id, b"abc");
        std::fs::write(t.root.join("Eingang/neu.txt"), "meanwhile").unwrap();
        let r = t.up.finish(&t.vols, &s.id, OWNER, None, true, 1).unwrap();
        assert!(matches!(r, Finished::Clash(_)));
        let r =
            t.up.finish(&t.vols, &s.id, OWNER, Some(Clash::Skip), true, 1)
                .unwrap();
        assert!(matches!(r, Finished::Skipped));
        assert_eq!(
            std::fs::read(t.root.join("Eingang/neu.txt")).unwrap(),
            b"meanwhile"
        );
        assert_eq!(t.parts(), 0);
        // Replacing without delete rights only asks again.
        let Created::Started(s) = t.start("neu.txt", b"abc", Clash::Replace, None) else {
            panic!()
        };
        t.send_all(&s.id, b"abc");
        assert!(matches!(
            t.up.finish(&t.vols, &s.id, OWNER, None, false, 1).unwrap(),
            Finished::Clash(_)
        ));
    }

    #[test]
    fn resume_after_restart_trusts_only_synced_chunks() {
        let t = setup("");
        let data = vec![7u8; 10];
        let Created::Started(s) = t.start("big.bin", &data, Clash::Ask, None) else {
            panic!()
        };
        // Pretend chunks 0 and 1 were synced and recorded before a crash.
        let mut rec = t.db.upload(&s.id).unwrap().unwrap();
        rec.have = vec![(0, 2)];
        t.db.put_upload(&rec, true).unwrap();
        let fresh = Uploads::new(t.db.clone());
        let st = fresh.status(&t.vols, &s.id, OWNER).unwrap();
        assert_eq!(st.have, [0, 1]);
        let tk = fresh.begin_chunk(&t.vols, &s.id, OWNER, 2, None).unwrap();
        tk.write_at(&data[8..], 0).unwrap();
        tk.complete(1).unwrap();
        // The first chunks were never really written in this test, so content is zeros there.
        assert!(matches!(
            fresh.finish(&t.vols, &s.id, OWNER, None, true, 1).unwrap(),
            Finished::Done { .. }
        ));
        assert_eq!(
            std::fs::read(t.root.join("Eingang/big.bin")).unwrap().len(),
            10
        );
    }

    #[test]
    fn cancel_and_missing_part() {
        let t = setup("");
        let Created::Started(s) = t.start("x", b"12345", Clash::Ask, None) else {
            panic!()
        };
        assert_eq!(t.parts(), 1);
        t.up.cancel(&t.vols, &s.id, OWNER).unwrap();
        assert_eq!(t.parts(), 0);
        assert!(matches!(
            t.up.status(&t.vols, &s.id, OWNER),
            Err(OpError::NotFound)
        ));

        let Created::Started(s) = t.start("y", b"12345", Clash::Ask, None) else {
            panic!()
        };
        std::fs::remove_file(t.root.join(UPLOADS_DIR).join(part_name(&s.id))).unwrap();
        let fresh = Uploads::new(t.db.clone());
        assert!(matches!(
            fresh.status(&t.vols, &s.id, OWNER),
            Err(OpError::Gone)
        ));
        assert!(t.db.upload(&s.id).unwrap().is_none());
    }

    #[test]
    fn empty_files() {
        let t = setup("");
        let Created::Started(s) = t.start("leer.txt", b"", Clash::Ask, None) else {
            panic!()
        };
        assert_eq!(s.chunks, 0);
        t.up.finish(&t.vols, &s.id, OWNER, None, true, 1).unwrap();
        assert_eq!(std::fs::read(t.root.join("Eingang/leer.txt")).unwrap(), b"");
    }

    #[test]
    fn staged_folder_uploads_appear_at_once() {
        let t = setup("stage_folder_uploads = true");
        let b = t.up.create_batch(t.vol(), &p("Eingang"), OWNER, 1).unwrap();
        assert!(b.staged);
        for (name, data) in [("Scan/a.pdf", b"aaa".as_slice()), ("Scan/sub/b.pdf", b"bb")] {
            let Created::Started(s) = t.start(name, data, Clash::Ask, Some(&b)) else {
                panic!()
            };
            t.send_all(&s.id, data);
            t.up.finish(&t.vols, &s.id, OWNER, None, true, 2).unwrap();
            assert!(
                !t.root.join("Eingang/Scan").exists(),
                "nothing visible before the batch is done"
            );
        }
        // A loose file in the same batch lands directly.
        let Created::Started(s) = t.start("lose.txt", b"l", Clash::Ask, Some(&b)) else {
            panic!()
        };
        t.send_all(&s.id, b"l");
        t.up.finish(&t.vols, &s.id, OWNER, None, true, 2).unwrap();
        assert!(t.root.join("Eingang/lose.txt").is_file());

        std::fs::create_dir(t.root.join("Eingang/Scan")).unwrap();
        let moved = t.up.finish_batch(&t.vols, &b.id, OWNER).unwrap();
        assert_eq!(moved, [("Scan".to_string(), "Scan (1)".to_string())]);
        assert_eq!(
            std::fs::read(t.root.join("Eingang/Scan (1)/sub/b.pdf")).unwrap(),
            b"bb"
        );
        assert_eq!(t.parts(), 0);
        assert!(t.db.batches().unwrap().is_empty());
    }

    #[test]
    fn a_batch_waits_for_its_uploads() {
        let t = setup("stage_folder_uploads = true");
        let b = t.up.create_batch(t.vol(), &p("Eingang"), OWNER, 1).unwrap();
        let Created::Started(s) = t.start("F/x", b"x", Clash::Ask, Some(&b)) else {
            panic!()
        };
        assert!(matches!(
            t.up.finish_batch(&t.vols, &b.id, OWNER),
            Err(OpError::Incomplete)
        ));
        t.up.cancel(&t.vols, &s.id, OWNER).unwrap();
        assert!(t.up.finish_batch(&t.vols, &b.id, OWNER).unwrap().is_empty());
    }

    #[test]
    fn sweep_removes_abandoned_parts_and_trashes_abandoned_staging() {
        let t = setup("stage_folder_uploads = true");
        let day = 86_400;
        let b = t.up.create_batch(t.vol(), &p("Eingang"), OWNER, 0).unwrap();
        let Created::Started(s) = t.start("F/done.txt", b"ok", Clash::Ask, Some(&b)) else {
            panic!()
        };
        t.send_all(&s.id, b"ok");
        t.up.finish(&t.vols, &s.id, OWNER, None, true, 0).unwrap();
        let Created::Started(open) = t.start("F/half.bin", b"12345678", Clash::Ask, Some(&b))
        else {
            panic!()
        };
        // Make everything old.
        for mut rec in t.db.uploads().unwrap() {
            rec.touched = 0;
            t.db.put_upload(&rec, true).unwrap();
        }
        let mut br = t.db.batch(&b.id).unwrap().unwrap();
        br.touched = 0;
        t.db.put_batch(&br).unwrap();
        let fresh = Uploads::new(t.db.clone());

        assert_eq!(fresh.sweep(&t.vols, 7 * day, 3 * day), (0, 0), "too young");
        assert_eq!(fresh.sweep(&t.vols, 7 * day, 8 * day), (1, 1));
        assert!(t.db.upload(&open.id).unwrap().is_none());
        assert_eq!(t.parts(), 0);
        let trashed = t.db.trash_items().unwrap();
        assert_eq!(trashed.len(), 1);
        assert_eq!(
            (trashed[0].path.as_str(), trashed[0].reason.as_str()),
            ("Eingang/F", "unfinished_upload")
        );
        assert!(!t.root.join("Eingang/F").exists());
        trash::restore(&t.db, t.vol(), &trashed[0]).unwrap();
        assert_eq!(
            std::fs::read(t.root.join("Eingang/F/done.txt")).unwrap(),
            b"ok"
        );
    }

    #[test]
    fn ranges_round_trip() {
        let have = [true, true, false, true, false, false, true];
        let r = to_ranges(&have);
        assert_eq!(r, [(0, 2), (3, 4), (6, 7)]);
        assert_eq!(from_ranges(&r, 7), have);
        assert_eq!(chunk_count(0, 4), 0);
        assert_eq!(chunk_count(8, 4), 2);
        assert_eq!(chunk_count(9, 4), 3);
    }
}
