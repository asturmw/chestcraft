//! Thumbnails: two sizes on the longest edge (grid and viewer), aspect ratio kept, never
//! larger than the source picture. Sources: raster images (decoded in process, EXIF
//! orientation applied), HEIC/AVIF and video frames (ffmpeg), STL and 3MF (the slicer
//! thumbnail embedded in the 3MF, else a software-rendered mesh on a transparent
//! background), G-code and binary G-code (the embedded slicer thumbnail) and SVG (resvg,
//! never loading anything outside the file).
//!
//! Results land in a disk cache keyed by the file's identity (device, inode, size, mtime),
//! so a changed file gets a new thumbnail and a moved one keeps its old. Failures are cached
//! as empty files, so a broken file isn't decoded again on every page load. Concurrent
//! requests for the same thumbnail wait for one render. Renders run on a few blocking
//! threads; ffmpeg runs at most `thumbnails.max_ffmpeg` times in parallel.

mod cache;
mod gcode;
mod mesh;
pub(crate) mod raster;
mod svg;
mod threemf;
mod video;

use crate::config::Config;
pub use crate::fs::filetype::ThumbSource;
use crate::fs::volume::Meta;
use cache::{Cache, Lookup};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io;
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;

/// Longest edge of grid thumbnails (tiles at device pixel ratio 2).
pub const GRID_PX: u32 = 480;
/// Longest edge of the viewer's picture; the original loads on "full size".
pub const VIEW_PX: u32 = 2048;
/// Bumped when a renderer changes, so old cache entries are no longer found.
const RENDER_VERSION: u32 = 1;

pub const FFMPEG: &str = "/usr/bin/ffmpeg";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    Grid,
    View,
}

impl Size {
    pub fn px(self) -> u32 {
        match self {
            Size::Grid => GRID_PX,
            Size::View => VIEW_PX,
        }
    }

    /// The `thumb` query value: empty or `grid`, or `view`.
    pub fn parse(s: &str) -> Option<Size> {
        match s {
            "" | "grid" => Some(Size::Grid),
            "view" => Some(Size::View),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Thumb {
    pub bytes: Vec<u8>,
    pub mime: &'static str,
}

#[derive(Debug, thiserror::Error)]
pub enum ThumbError {
    /// The file can't give a thumbnail (corrupt, unsupported variant, too large). Cached.
    #[error("{0}")]
    Unusable(String),
    /// Reading failed; may work next time. Not cached.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// Not possible on this machine right now (no ffmpeg, shutting down). Not cached.
    #[error("{0}")]
    Skipped(&'static str),
    /// The visitor asked for too many new thumbnails.
    #[error("too many thumbnail requests")]
    Busy,
}

pub type Result<T> = std::result::Result<T, ThumbError>;

pub(crate) fn unusable(msg: impl Into<String>) -> ThumbError {
    ThumbError::Unusable(msg.into())
}

type Key = [u8; 16];

fn key(meta: &Meta, src: ThumbSource, size: Size) -> Key {
    let mut h = Sha256::new();
    h.update(
        format!(
            "v{RENDER_VERSION}:{}:{}:{}:{}.{}:{}:{}",
            meta.dev,
            meta.ino,
            meta.size,
            meta.mtime,
            meta.mtime_nsec,
            src.tag(),
            size.px()
        )
        .as_bytes(),
    );
    let mut k = [0; 16];
    k.copy_from_slice(&h.finalize()[..16]);
    k
}

/// Short tag that changes whenever the file changes: thumbnail URLs carry it as `v=`, so
/// browsers may cache them for good.
pub fn version_tag(meta: &Meta) -> String {
    let k = key(meta, ThumbSource::Image, Size::Grid);
    cache::hex(&k[..6])
}

pub struct Thumbs {
    cache: Arc<Cache>,
    cpu: Semaphore,
    ffmpeg: Semaphore,
    ffmpeg_path: PathBuf,
    inflight: Mutex<HashMap<Key, Arc<tokio::sync::Mutex<()>>>>,
}

impl Thumbs {
    pub fn new(cfg: &Config) -> Thumbs {
        let cpus = std::thread::available_parallelism().map_or(2, |n| n.get());
        Thumbs {
            cache: Arc::new(Cache::new(
                cfg.paths.cache_dir.join("thumbs"),
                u64::from(cfg.thumbnails.cache_gib) << 30,
            )),
            cpu: Semaphore::new((cpus / 2).clamp(1, 4)),
            ffmpeg: Semaphore::new(cfg.thumbnails.max_ffmpeg.max(1) as usize),
            ffmpeg_path: PathBuf::from(FFMPEG),
            inflight: Mutex::new(HashMap::new()),
        }
    }

    /// Tests point this at a fake or missing ffmpeg.
    #[cfg(test)]
    pub fn set_ffmpeg(&mut self, path: PathBuf) {
        self.ffmpeg_path = path;
    }

    /// Count what the cache holds and trim it to its size (in the background).
    pub fn start(&self) {
        let cache = self.cache.clone();
        tokio::task::spawn_blocking(move || cache.trim());
    }

    /// The thumbnail of the open file `fd`. `Ok(None)` when the file has none. A thumbnail
    /// that isn't cached yet is rendered only if `may_render` allows it.
    pub async fn get(
        &self,
        fd: OwnedFd,
        meta: &Meta,
        src: ThumbSource,
        size: Size,
        may_render: impl FnOnce() -> bool,
    ) -> Result<Option<Thumb>> {
        let key = key(meta, src, size);
        match self.cache.lookup(key).await {
            Lookup::Hit(t) => return Ok(Some(t)),
            Lookup::Negative => return Ok(None),
            Lookup::Miss => {}
        }
        if !may_render() {
            return Err(ThumbError::Busy);
        }
        let slot = self
            .inflight
            .lock()
            .unwrap()
            .entry(key)
            .or_default()
            .clone();
        let result = {
            let _one = slot.lock().await;
            match self.cache.lookup(key).await {
                Lookup::Hit(t) => Ok(Some(t)),
                Lookup::Negative => Ok(None),
                Lookup::Miss => self.render_and_store(key, fd, meta.size, src, size).await,
            }
        };
        {
            let mut map = self.inflight.lock().unwrap();
            if map.get(&key).is_some_and(|s| Arc::strong_count(s) <= 2) {
                map.remove(&key);
            }
        }
        result
    }

    async fn render_and_store(
        &self,
        key: Key,
        fd: OwnedFd,
        len: u64,
        src: ThumbSource,
        size: Size,
    ) -> Result<Option<Thumb>> {
        match self.render(fd, len, src, size).await {
            Ok(t) => {
                self.cache.store(key, t.bytes.clone()).await;
                Ok(Some(t))
            }
            Err(ThumbError::Unusable(why)) => {
                tracing::debug!(source = src.tag(), "no thumbnail: {why}");
                self.cache.store(key, Vec::new()).await;
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    async fn render(&self, fd: OwnedFd, len: u64, src: ThumbSource, size: Size) -> Result<Thumb> {
        let px = size.px();
        if src.uses_ffmpeg() {
            let frame = {
                let _p = self
                    .ffmpeg
                    .acquire()
                    .await
                    .map_err(|_| ThumbError::Skipped("closing"))?;
                video::frame(&self.ffmpeg_path, fd, src == ThumbSource::Video, px).await?
            };
            let _c = self
                .cpu
                .acquire()
                .await
                .map_err(|_| ThumbError::Skipped("closing"))?;
            return blocking(move || raster::encode(raster::decode_bytes(&frame)?, px)).await;
        }
        let _c = self
            .cpu
            .acquire()
            .await
            .map_err(|_| ThumbError::Skipped("closing"))?;
        blocking(move || render_file(std::fs::File::from(fd), len, src, px)).await
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    match tokio::task::spawn_blocking(f).await {
        Ok(r) => r,
        Err(_) => Err(unusable("renderer crashed")),
    }
}

fn render_file(file: std::fs::File, len: u64, src: ThumbSource, px: u32) -> Result<Thumb> {
    let img = match src {
        ThumbSource::Image => raster::decode(io::BufReader::new(file))?,
        ThumbSource::Stl => mesh::render(&mesh::read_stl(file, len)?, px)?.into(),
        ThumbSource::ThreeMf => match threemf::read(file)? {
            threemf::Content::Picture(bytes) => raster::decode_bytes(&bytes)?,
            threemf::Content::Mesh(m) => mesh::render(&m, px)?.into(),
        },
        ThumbSource::Gcode => raster::decode_bytes(&gcode::text_thumbnail(file)?)?,
        ThumbSource::Bgcode => raster::decode_bytes(&gcode::binary_thumbnail(file)?)?,
        ThumbSource::Svg => svg::render(file, len, px)?.into(),
        ThumbSource::Still | ThumbSource::Video => return Err(unusable("needs ffmpeg")),
    };
    raster::encode(img, px)
}

#[cfg(test)]
mod tests;
