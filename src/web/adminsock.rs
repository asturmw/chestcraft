//! The admin socket (`paths.admin_socket`): CLI state commands while the server runs.
//!
//! One request per connection: a JSON line `{"version": …, "cmd": …}` ([`Request`]), then
//! the server answers with the command's [`Out`] as JSON and closes. Only root and the
//! service user get through: the socket is mode 600 in a folder of the service, and the
//! caller's uid is checked again (`SO_PEERCRED`). The CLI and the server must be the same
//! version, because the command travels as serialized clap structs.

use super::Shared;
use crate::cli::{self, AdminCmd, Out};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

const MAX_REQUEST: u64 = 64 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub version: String,
    pub cmd: AdminCmd,
}

/// Bind the socket. `Ok(None)`: its folder doesn't exist (not run by systemd), so CLI
/// commands open the database themselves while the server is stopped.
pub fn bind(path: &Path) -> io::Result<Option<UnixListener>> {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    if !path.parent().is_some_and(Path::is_dir) {
        return Ok(None);
    }
    match std::fs::symlink_metadata(path) {
        // Left over from a crash or the previous image of a restart: this process holds the
        // database lock, so no other server uses it.
        Ok(m) if m.file_type().is_socket() => std::fs::remove_file(path)?,
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} exists and isn't a socket", path.display()),
            ));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(Some(listener))
}

/// Answer CLI commands until `shutdown`; then remove the socket.
pub async fn serve(
    st: Shared,
    listener: UnixListener,
    path: PathBuf,
    shutdown: tokio_util::sync::CancellationToken,
) {
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    tokio::spawn(handle(st.clone(), stream));
                }
                Err(e) => {
                    tracing::warn!("admin socket: {e}");
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            },
        }
    }
    drop(listener);
    let _ = std::fs::remove_file(&path);
}

async fn handle(st: Shared, mut stream: UnixStream) {
    let uid = match stream.peer_cred() {
        Ok(c) => c.uid(),
        Err(e) => {
            tracing::warn!("admin socket: no peer credentials: {e}");
            return;
        }
    };
    let me = rustix::process::geteuid().as_raw();
    let out = if uid != 0 && uid != me {
        tracing::warn!(
            uid,
            "admin socket: refused a caller that is neither root nor the service user"
        );
        failure("only root and the service user may send commands")
    } else {
        match read_request(&mut stream).await {
            Ok(req) if req.version != crate::VERSION => failure(&format!(
                "the server runs chestcraft {}, this is {}: restart the service (or use the \
                 matching binary)",
                crate::VERSION,
                req.version
            )),
            Ok(req) => {
                let label = req.cmd.label();
                let st2 = st.clone();
                let ran = tokio::task::spawn_blocking(move || {
                    let env = cli::Env {
                        cfg: &st2.cfg,
                        db: &st2.db,
                        config_file: st2.config_file.as_deref(),
                    };
                    cli::run(req.cmd, &env, super::now())
                })
                .await;
                match ran {
                    Ok(out) => {
                        tracing::info!(uid, command = label, code = out.code, "CLI command");
                        out
                    }
                    Err(_) => failure("the command crashed; see the server's log"),
                }
            }
            Err(e) => failure(&e),
        }
    };
    let mut line = serde_json::to_vec(&out).unwrap_or_default();
    line.push(b'\n');
    let _ = stream.write_all(&line).await;
    let _ = stream.shutdown().await;
}

async fn read_request(stream: &mut UnixStream) -> Result<Request, String> {
    let mut line = String::new();
    let mut reader = BufReader::new((&mut *stream).take(MAX_REQUEST));
    match tokio::time::timeout(READ_TIMEOUT, reader.read_line(&mut line)).await {
        Err(_) => return Err("no request within 10 s".into()),
        Ok(Err(e)) => return Err(format!("reading the request: {e}")),
        Ok(Ok(_)) => {}
    }
    serde_json::from_str(&line).map_err(|e| {
        format!("can't read the request ({e}); are the CLI and the server the same version?")
    })
}

fn failure(msg: &str) -> Out {
    Out {
        stdout: String::new(),
        stderr: format!("{msg}\n"),
        code: 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Remote, UserCmd};

    #[tokio::test]
    async fn commands_run_in_the_server() {
        let (st, _dir) = super::super::tests::state_for_tests();
        let sock_dir = tempfile::tempdir().unwrap();
        let path = sock_dir.path().join("admin.sock");
        let listener = bind(&path).unwrap().unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let token = tokio_util::sync::CancellationToken::new();
        let task = tokio::spawn(serve(st.clone(), listener, path.clone(), token.clone()));

        let p = path.clone();
        let add = AdminCmd::User(UserCmd::Add {
            name: "Kim".into(),
            role: "admin".into(),
        });
        let out = tokio::task::spawn_blocking(move || cli::send(&p, &add))
            .await
            .unwrap()
            .unwrap();
        let Remote::Done(out) = out else {
            panic!("no server")
        };
        assert_eq!(out.code, 0, "{out:?}");
        assert!(out.stdout.contains("One-time password"), "{out:?}");
        let user = st.db.user("kim").unwrap().unwrap();
        assert!(user.must_change_password);
        assert!(
            st.db
                .recent_audit(5)
                .unwrap()
                .iter()
                .any(|e| e.action == "user_created" && e.actor == "cli")
        );

        // A second add of the same name fails and changes nothing.
        let p = path.clone();
        let again = AdminCmd::User(UserCmd::Add {
            name: "KIM".into(),
            role: "admin".into(),
        });
        let Remote::Done(out) = tokio::task::spawn_blocking(move || cli::send(&p, &again))
            .await
            .unwrap()
            .unwrap()
        else {
            panic!("no server")
        };
        assert_eq!(out.code, 1);
        assert!(out.stderr.contains("exists already"), "{out:?}");
        assert_eq!(st.db.user("kim").unwrap().unwrap(), user);

        // Another version is turned away.
        let p = path.clone();
        let raw = tokio::task::spawn_blocking(move || {
            use std::io::{Read, Write};
            let mut s = std::os::unix::net::UnixStream::connect(&p).unwrap();
            s.write_all(br#"{"version":"0.0.0","cmd":{"User":"List"}}"#)
                .unwrap();
            s.write_all(b"\n").unwrap();
            let mut text = String::new();
            s.read_to_string(&mut text).unwrap();
            text
        })
        .await
        .unwrap();
        let out: Out = serde_json::from_str(&raw).unwrap();
        assert_eq!(out.code, 1);
        assert!(out.stderr.contains("restart the service"), "{out:?}");

        // Shutdown removes the socket; then the CLI finds no server.
        token.cancel();
        task.await.unwrap();
        assert!(!path.exists());
        let p = path.clone();
        let r = tokio::task::spawn_blocking(move || cli::send(&p, &AdminCmd::Backup))
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(r, Remote::NoServer));
    }

    #[test]
    fn bind_replaces_a_stale_socket_but_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("admin.sock");
        let stale = std::os::unix::net::UnixListener::bind(&path).unwrap();
        drop(stale);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            assert!(bind(&path).unwrap().is_some());
            let file = dir.path().join("file");
            std::fs::write(&file, b"x").unwrap();
            assert!(bind(&file).is_err());
            assert_eq!(std::fs::read(&file).unwrap(), b"x");
            assert!(
                bind(&dir.path().join("absent/admin.sock"))
                    .unwrap()
                    .is_none()
            );
        });
    }
}
