//! Running the storage tools: by absolute path, with an empty environment, a timeout and
//! bounded output. A tool that hangs (a stuck pool can leave `zpool` in uninterruptible
//! sleep) is not started again until the hanging run has exited, so they don't pile up.

use std::collections::HashSet;
use std::process::Stdio;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;
use tokio::io::AsyncReadExt;

/// Largest output read from a tool.
const MAX_OUTPUT: u64 = 64 << 20;

static HANGING: LazyLock<Mutex<HashSet<&'static str>>> = LazyLock::new(Default::default);

pub struct Output {
    /// Exit code (None when killed by a signal).
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    /// First line of stderr, for error messages.
    pub stderr: String,
}

impl Output {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    /// stdout on success, else an error with the tool's first stderr line.
    pub fn into_stdout(self, prog: &str) -> Result<Vec<u8>, String> {
        if self.success() {
            Ok(self.stdout)
        } else {
            Err(match (self.code, self.stderr.is_empty()) {
                (_, false) => format!("{prog}: {}", self.stderr),
                (Some(c), true) => format!("{prog} exited with status {c}"),
                (None, true) => format!("{prog} was killed"),
            })
        }
    }
}

async fn read_limited(r: Option<impl tokio::io::AsyncRead + Unpin>) -> Vec<u8> {
    let mut out = Vec::new();
    if let Some(r) = r {
        let _ = r.take(MAX_OUTPUT).read_to_end(&mut out).await;
    }
    out
}

/// Run `prog args…` and wait at most `timeout`.
pub async fn run(prog: &'static str, args: &[&str], timeout: Duration) -> Result<Output, String> {
    if HANGING.lock().unwrap().contains(prog) {
        return Err(format!("an earlier {prog} is still hanging"));
    }
    let mut child = tokio::process::Command::new(prog)
        .args(args)
        .env_clear()
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("can't run {prog}: {e}"))?;
    let out = tokio::spawn(read_limited(child.stdout.take()));
    let err = tokio::spawn(read_limited(child.stderr.take()));
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => {
            let stdout = out.await.unwrap_or_default();
            let stderr = err.await.unwrap_or_default();
            let stderr = String::from_utf8_lossy(&stderr)
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("")
                .chars()
                .take(300)
                .collect();
            Ok(Output {
                code: status.code(),
                stdout,
                stderr,
            })
        }
        Ok(Err(e)) => Err(format!("{prog}: {e}")),
        Err(_) => {
            let _ = child.start_kill();
            HANGING.lock().unwrap().insert(prog);
            tokio::spawn(async move {
                let _ = child.wait().await;
                HANGING.lock().unwrap().remove(prog);
            });
            Err(format!(
                "{prog} didn't answer within {} s",
                timeout.as_secs()
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn output_errors_and_timeouts() {
        let o = run(
            "/bin/sh",
            &["-c", "echo hi; echo oops >&2; exit 3"],
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert_eq!(o.code, Some(3));
        assert_eq!(o.stdout, b"hi\n");
        assert_eq!(o.into_stdout("sh").unwrap_err(), "sh: oops");
        // The environment is empty.
        let o = run(
            "/bin/sh",
            &["-c", "echo \"$HOME|$LC_ALL\""],
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert_eq!(o.stdout, b"|C\n");
        let e = run("/bin/sleep", &["5"], Duration::from_millis(100))
            .await
            .err()
            .unwrap();
        assert!(e.contains("didn't answer"), "{e}");
        assert!(
            run("/nonexistent/tool", &[], Duration::from_secs(1))
                .await
                .is_err()
        );
    }
}
