//! Folder sizes for listings: `GET <folder>/?du[&hidden=1]` →
//! `{"bytes":N,"files":n,"dirs":n,"complete":true}` (`fs::tree::usage`; `complete` is false
//! when the walk stopped early, then the folder holds at least that much). The listing's
//! script asks for each folder row after the page has loaded.
//!
//! At most two walks run at a time, each stops after [`MAX_ENTRIES`] entries or
//! [`MAX_TIME`], and results are kept for [`TTL`], keyed by the folder's inode and
//! modification time (which changes when something is added to or removed from the folder
//! itself; deeper changes show after the TTL).

use crate::fs::path::RelPath;
use crate::fs::tree::{Usage, UsageLimits, usage};
use crate::fs::volume::VolumeRoot;
use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

pub const TTL: Duration = Duration::from_secs(300);
pub const MAX_ENTRIES: u64 = 2_000_000;
pub const MAX_TIME: Duration = Duration::from_secs(15);
const MAX_CACHED: usize = 20_000;
const PARALLEL: usize = 2;

type Key = (u64, u64, i64, u32, bool);

pub struct FolderSizes {
    walks: Semaphore,
    cache: Mutex<HashMap<Key, (Instant, Usage)>>,
}

impl Default for FolderSizes {
    fn default() -> Self {
        FolderSizes {
            walks: Semaphore::new(PARALLEL),
            cache: Mutex::new(HashMap::new()),
        }
    }
}

impl FolderSizes {
    /// The size of `rel` (a folder in `vol`), from the cache or a new walk.
    pub async fn get(
        &self,
        vol: Arc<VolumeRoot>,
        rel: RelPath,
        include_hidden: bool,
    ) -> io::Result<Usage> {
        let meta = {
            let (vol, rel) = (vol.clone(), rel.clone());
            tokio::task::spawn_blocking(move || vol.stat(&rel))
                .await
                .map_err(io::Error::other)??
        };
        let key = (
            meta.dev,
            meta.ino,
            meta.mtime,
            meta.mtime_nsec,
            include_hidden,
        );
        if let Some(u) = self.cached(&key) {
            return Ok(u);
        }
        let _walk = self.walks.acquire().await.map_err(io::Error::other)?;
        // Another request may have walked it while this one waited.
        if let Some(u) = self.cached(&key) {
            return Ok(u);
        }
        let u = tokio::task::spawn_blocking(move || {
            let limits = UsageLimits {
                max_entries: MAX_ENTRIES,
                deadline: Instant::now() + MAX_TIME,
            };
            usage(&vol, &rel, include_hidden, &limits)
        })
        .await
        .map_err(io::Error::other)??;
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() >= MAX_CACHED {
            let now = Instant::now();
            cache.retain(|_, (at, _)| now.duration_since(*at) < TTL);
            if cache.len() >= MAX_CACHED {
                cache.clear();
            }
        }
        cache.insert(key, (Instant::now(), u));
        Ok(u)
    }

    fn cached(&self, key: &Key) -> Option<Usage> {
        let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache
            .get(key)
            .filter(|(at, _)| at.elapsed() < TTL)
            .map(|(_, u)| *u)
    }
}
