//! Frames through ffmpeg (videos, and stills the in-process decoders can't read: HEIC,
//! AVIF). ffmpeg gets the already opened file as stdin and reads it as `file:/proc/self/fd/0`,
//! a fresh, seekable descriptor of the same file; it never sees a path. Only container
//! demuxers are allowed (no playlists, no concat lists that could point at other files or
//! URLs) and only the `file` protocol. ffmpeg runs with an empty environment and is killed
//! after a timeout.

use super::{Result, ThumbError, unusable};
use std::os::fd::OwnedFd;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

const TIMEOUT: Duration = Duration::from_secs(60);
const MAX_OUTPUT: u64 = 64 << 20;
/// Videos: the frame at this offset (seconds), or the first one for shorter videos.
const SEEK: &str = "3";
/// Demuxers ffmpeg may use on user files.
const FORMATS: &str = "mov,matroska,webm,avi,asf,flv,mpegts,mpeg,mpegps,ogg,dv,mxf,\
    h264,hevc,m4v,rawvideo,yuv4mpegpipe";

/// One frame as PNG, at most `px` on the longest edge.
pub async fn frame(ffmpeg: &Path, fd: OwnedFd, video: bool, px: u32) -> Result<Vec<u8>> {
    if !ffmpeg.is_file() {
        return Err(ThumbError::Skipped("ffmpeg is not installed"));
    }
    let fd2 = fd.try_clone()?;
    if video
        && let Ok(out) = run(ffmpeg, fd, Some(SEEK), px).await
        && !out.is_empty()
    {
        return Ok(out);
    }
    let out = run(ffmpeg, fd2, None, px).await?;
    if out.is_empty() {
        return Err(unusable("ffmpeg produced no frame"));
    }
    Ok(out)
}

async fn run(ffmpeg: &Path, fd: OwnedFd, seek: Option<&str>, px: u32) -> Result<Vec<u8>> {
    let scale = format!("scale='min(iw,{px})':'min(ih,{px})':force_original_aspect_ratio=decrease");
    let mut cmd = Command::new(ffmpeg);
    cmd.env_clear()
        .args(["-nostdin", "-hide_banner", "-loglevel", "error"])
        .args(["-protocol_whitelist", "file", "-format_whitelist", FORMATS]);
    if let Some(s) = seek {
        cmd.args(["-ss", s]);
    }
    cmd.args(["-i", "file:/proc/self/fd/0"])
        // No -map: ffmpeg's own pick also assembles HEIF tile grids.
        .args(["-frames:v", "1", "-an", "-sn", "-dn"])
        .args(["-vf", &scale, "-pix_fmt", "rgb24"])
        .args(["-f", "image2pipe", "-c:v", "png", "pipe:1"])
        .stdin(Stdio::from(fd))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = cmd.spawn()?;
    let mut stdout = child.stdout.take().ok_or_else(|| unusable("no stdout"))?;
    let work = async {
        let mut out = Vec::new();
        (&mut stdout).take(MAX_OUTPUT).read_to_end(&mut out).await?;
        let status = child.wait().await?;
        Ok::<_, std::io::Error>((status, out))
    };
    match tokio::time::timeout(TIMEOUT, work).await {
        Ok(Ok((status, out))) if status.success() => Ok(out),
        Ok(Ok((status, _))) => Err(unusable(format!("ffmpeg failed: {status}"))),
        Ok(Err(e)) => Err(e.into()),
        Err(_) => Err(unusable("ffmpeg timed out")),
    }
}
