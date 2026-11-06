//! Disk cache of rendered thumbnails: `<dir>/<2 hex>/<32 hex>`, an empty file for "this
//! file has no thumbnail". Least recently used entries go first once the cache outgrows
//! its size: a hit refreshes the entry's mtime (at most once a day), and the trim removes
//! the oldest mtimes until 90 % of the limit is left. Everything here is cache data, never
//! a user file.

use super::{Key, Thumb};
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

const REFRESH_AFTER: Duration = Duration::from_secs(86_400);
/// Temporary files older than this are left over from a crash.
const STALE_TMP: Duration = Duration::from_secs(3600);

pub enum Lookup {
    Hit(Thumb),
    Negative,
    Miss,
}

pub struct Cache {
    dir: PathBuf,
    cap: u64,
    used: AtomicU64,
    trimming: AtomicBool,
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Content type from the first bytes (the cache stores bare image files).
fn sniff(b: &[u8]) -> &'static str {
    if b.starts_with(b"\x89PNG") {
        "image/png"
    } else {
        "image/jpeg"
    }
}

impl Cache {
    pub fn new(dir: PathBuf, cap: u64) -> Cache {
        Cache {
            dir,
            cap,
            used: AtomicU64::new(0),
            trimming: AtomicBool::new(false),
        }
    }

    fn path(&self, key: Key) -> PathBuf {
        let h = hex(&key);
        self.dir.join(&h[..2]).join(h)
    }

    pub async fn lookup(&self, key: Key) -> Lookup {
        let path = self.path(key);
        tokio::task::spawn_blocking(move || lookup_blocking(&path))
            .await
            .unwrap_or(Lookup::Miss)
    }

    /// Store an entry (empty = no thumbnail). Failures are logged and otherwise ignored:
    /// the thumbnail is simply rendered again next time.
    pub async fn store(self: &Arc<Self>, key: Key, bytes: Vec<u8>) {
        let (cache, path) = (self.clone(), self.path(key));
        let _ = tokio::task::spawn_blocking(move || {
            let len = bytes.len() as u64;
            if let Err(e) = write_atomic(&path, &bytes) {
                tracing::warn!(path = %path.display(), "can't store thumbnail: {e}");
                return;
            }
            let used = cache.used.fetch_add(len, Ordering::Relaxed) + len;
            if used > cache.cap {
                cache.trim();
            }
        })
        .await;
    }

    /// Count the cache and remove the least recently used entries while it is larger than
    /// its limit. Only one trim runs at a time.
    pub fn trim(&self) {
        if self.trimming.swap(true, Ordering::AcqRel) {
            return;
        }
        let (total, removed) = trim_dir(&self.dir, self.cap);
        self.used.store(total, Ordering::Relaxed);
        if removed > 0 {
            tracing::info!(removed, kept_bytes = total, "trimmed thumbnail cache");
        }
        self.trimming.store(false, Ordering::Release);
    }

    #[cfg(test)]
    pub fn used(&self) -> u64 {
        self.used.load(Ordering::Relaxed)
    }
}

fn lookup_blocking(path: &Path) -> Lookup {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Lookup::Miss,
        Err(e) => {
            tracing::warn!(path = %path.display(), "can't read cached thumbnail: {e}");
            return Lookup::Miss;
        }
    };
    let stale = fs::metadata(path)
        .and_then(|m| m.modified())
        .is_ok_and(|t| t.elapsed().is_ok_and(|age| age > REFRESH_AFTER));
    if stale && let Ok(f) = fs::File::options().write(true).open(path) {
        let _ = f.set_modified(SystemTime::now());
    }
    if bytes.is_empty() {
        Lookup::Negative
    } else {
        Lookup::Hit(Thumb {
            mime: sniff(&bytes),
            bytes,
        })
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let tmp = dir.join(format!(
        "{}.tmp.{:08x}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("t"),
        rand::random::<u32>()
    ));
    let res = (|| {
        let mut f = fs::File::options()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(bytes)?;
        fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res
}

/// Returns (bytes kept, entries removed).
fn trim_dir(dir: &Path, cap: u64) -> (u64, usize) {
    let mut entries: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
    let mut total = 0u64;
    let Ok(subs) = fs::read_dir(dir) else {
        return (0, 0);
    };
    for sub in subs.flatten() {
        let Ok(files) = fs::read_dir(sub.path()) else {
            continue;
        };
        for f in files.flatten() {
            let Ok(m) = f.metadata() else { continue };
            if !m.is_file() {
                continue;
            }
            let mtime = m.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            let name = f.file_name();
            if name.to_string_lossy().contains(".tmp.") {
                if mtime.elapsed().is_ok_and(|a| a > STALE_TMP) {
                    let _ = fs::remove_file(f.path());
                }
                continue;
            }
            total += m.len();
            entries.push((mtime, m.len(), f.path()));
        }
    }
    let mut removed = 0;
    if total > cap {
        let target = cap / 10 * 9;
        entries.sort_by_key(|e| e.0);
        for (_, len, path) in entries {
            if total <= target {
                break;
            }
            if fs::remove_file(&path).is_ok() {
                total -= len;
                removed += 1;
            }
        }
    }
    (total, removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stores_finds_and_trims_oldest_first() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = Arc::new(Cache::new(tmp.path().join("thumbs"), 1000));
        let k = |n: u8| [n; 16];
        cache.store(k(1), vec![0x89, b'P', b'N', b'G']).await;
        cache.store(k(2), Vec::new()).await;
        assert!(matches!(cache.lookup(k(1)).await, Lookup::Hit(t) if t.mime == "image/png"));
        assert!(matches!(cache.lookup(k(2)).await, Lookup::Negative));
        assert!(matches!(cache.lookup(k(3)).await, Lookup::Miss));

        // Age entry 3 and 4, then overflow the cache: the older one goes first.
        cache.store(k(3), vec![0xff; 400]).await;
        cache.store(k(4), vec![0xff; 400]).await;
        let old = SystemTime::now() - Duration::from_secs(10 * 86_400);
        let older = old - Duration::from_secs(86_400);
        fs::File::options()
            .write(true)
            .open(cache.path(k(3)))
            .unwrap()
            .set_modified(older)
            .unwrap();
        fs::File::options()
            .write(true)
            .open(cache.path(k(4)))
            .unwrap()
            .set_modified(old)
            .unwrap();
        cache.store(k(5), vec![0xff; 400]).await;
        assert!(matches!(cache.lookup(k(3)).await, Lookup::Miss));
        assert!(matches!(cache.lookup(k(4)).await, Lookup::Hit(_)));
        assert!(matches!(cache.lookup(k(5)).await, Lookup::Hit(_)));
        assert!(cache.used() <= 900, "{}", cache.used());
    }
}
