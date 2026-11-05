//! Bridges blocking writers (the zip walker) to streamed response bodies.

use super::limits::DownloadGuard;
use axum::body::Body;
use bytes::Bytes;
use std::io::{self, Write};
use tokio::sync::mpsc;

const CHUNK: usize = 256 * 1024;

/// A `Write` that sends 256 KiB chunks into a bounded channel. Blocks when the client reads
/// slowly; fails with BrokenPipe once the client is gone.
pub struct ChannelWriter {
    tx: mpsc::Sender<io::Result<Bytes>>,
    buf: Vec<u8>,
}

impl ChannelWriter {
    pub fn new(tx: mpsc::Sender<io::Result<Bytes>>) -> Self {
        ChannelWriter {
            tx,
            buf: Vec::with_capacity(CHUNK),
        }
    }

    fn send_buf(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let chunk = Bytes::from(std::mem::replace(&mut self.buf, Vec::with_capacity(CHUNK)));
        self.tx
            .blocking_send(Ok(chunk))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "client went away"))
    }
}

impl Write for ChannelWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(data);
        if self.buf.len() >= CHUNK {
            self.send_buf()?;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.send_buf()
    }
}

impl Drop for ChannelWriter {
    fn drop(&mut self) {
        let _ = self.send_buf();
    }
}

/// A response body fed by a channel. The guard (if any) is released when the body is done
/// or dropped.
pub fn body_from_channel(
    rx: mpsc::Receiver<io::Result<Bytes>>,
    guard: Option<DownloadGuard>,
) -> Body {
    let stream = futures_util::stream::unfold((rx, guard), |(mut rx, guard)| async move {
        rx.recv().await.map(|item| (item, (rx, guard)))
    });
    Body::from_stream(stream)
}
