//! JSON API for the upload client (`assets/upload.js`).
//!
//! ```text
//! POST   /.cc/upload                 {vol, dir, name, size, mtime?, clash?, batch?}
//!          201 {id, size, chunk, chunks, have}   started
//!          200 {skipped: true}                   clash = "skip" and the name is taken
//!          409 {error: "exists", clash: {…}}     clash = "ask" and the name is taken
//! GET    /.cc/upload/{id}            200 {id, size, chunk, chunks, have}
//! PUT    /.cc/upload/{id}/chunk/{n}  body = the chunk's bytes → 200 {have: <count>}
//! POST   /.cc/upload/{id}/done       {clash?} → 200 {path, renamed} | {skipped: true} | 409
//! DELETE /.cc/upload/{id}            204
//! POST   /.cc/batch                  {vol, dir} → 201 {id, staged}
//! POST   /.cc/batch/{id}/done        200 {moved: [[staged name, final name], …]}
//! ```
//!
//! Every call needs a session; changes also need the `X-CSRF-Token` header. Errors are
//! `{error: <code>, message: <text in the user's language>}`.

use super::auth::{audit, csrf_ok, now};
use super::views::Ctx;
use super::{ClientInfo, Current, CurrentUser, Shared, page_ctx};
use crate::auth::perm::{Perms, Principal, path_perms};
use crate::fs::ops::{Existing, OpError};
use crate::fs::path::{RelPath, validate_new_name};
use crate::fs::upload::{Clash, Created, Finished, NewUpload};
use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

/// Chunk data is written in pieces of at most this size.
const WRITE_BUF: usize = 4 << 20;

/// HTTP status, error code and message key for an operation error.
pub fn describe(err: &OpError) -> (StatusCode, &'static str, &'static str) {
    match err {
        OpError::NotFound => (StatusCode::NOT_FOUND, "not_found", "ops.not_found"),
        OpError::Exists(_) => (StatusCode::CONFLICT, "exists", "ops.exists"),
        OpError::Blocked(_) => (StatusCode::CONFLICT, "blocked", "ops.blocked"),
        OpError::NoSpace => (StatusCode::INSUFFICIENT_STORAGE, "no_space", "ops.no_space"),
        OpError::ReadOnlyInside => (
            StatusCode::CONFLICT,
            "read_only_inside",
            "ops.read_only_inside",
        ),
        OpError::VolumeInside => (StatusCode::CONFLICT, "volume_inside", "ops.volume_inside"),
        OpError::Denied => (StatusCode::FORBIDDEN, "forbidden", "ops.forbidden"),
        OpError::MountPoint => (StatusCode::CONFLICT, "mount_point", "ops.mount_point"),
        OpError::Busy => (StatusCode::CONFLICT, "busy", "ops.busy"),
        OpError::Incomplete => (StatusCode::CONFLICT, "incomplete", "ops.incomplete"),
        OpError::Gone => (StatusCode::GONE, "gone", "ops.gone"),
        OpError::Invalid(_) => (StatusCode::BAD_REQUEST, "invalid", "ops.invalid"),
        OpError::WorkDir { .. } => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "work_dir",
            "ops.work_dir",
        ),
        OpError::Io(_) | OpError::Db(_) => {
            (StatusCode::INTERNAL_SERVER_ERROR, "failed", "ops.failed")
        }
    }
}

/// The message for an operation error about the item `name`. Server-side failures are
/// logged here.
pub fn message(ctx: &Ctx, err: &OpError, name: &str) -> (StatusCode, &'static str, String) {
    let (status, code, key) = describe(err);
    if status.is_server_error() {
        tracing::error!(item = name, "operation failed: {err}");
    }
    let name = match err {
        OpError::Exists(ex) => ex.name.as_str(),
        OpError::Blocked(n) => n.as_str(),
        _ => name,
    };
    (status, code, ctx.tf(key, &[("name", name)]))
}

fn json_error(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(json!({ "error": code, "message": message }))).into_response()
}

fn op_error(ctx: &Ctx, err: &OpError, name: &str) -> Response {
    let (status, code, msg) = message(ctx, err, name);
    json_error(status, code, &msg)
}

fn invalid(ctx: &Ctx) -> Response {
    json_error(StatusCode::BAD_REQUEST, "invalid", ctx.t("ops.invalid"))
}

fn forbidden(ctx: &Ctx) -> Response {
    json_error(StatusCode::FORBIDDEN, "forbidden", ctx.t("ops.forbidden"))
}

fn internal(ctx: &Ctx) -> Response {
    json_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "failed",
        ctx.t("ops.failed"),
    )
}

/// The signed-in user behind an API call; changes also need Origin + CSRF.
fn api_user(
    st: &Shared,
    ctx: &Ctx,
    current: &Current,
    headers: &HeaderMap,
    change: bool,
) -> Result<Arc<CurrentUser>, Box<Response>> {
    let Some(cu) = current.clone() else {
        return Err(Box::new(json_error(
            StatusCode::UNAUTHORIZED,
            "signed_out",
            ctx.t("ops.signed_out"),
        )));
    };
    if change && !csrf_ok(headers, st, &cu, "") {
        return Err(Box::new(json_error(
            StatusCode::FORBIDDEN,
            "csrf",
            ctx.t("error.forbidden_body"),
        )));
    }
    Ok(cu)
}

fn actor(cu: &CurrentUser) -> String {
    format!("user:{}", cu.key)
}

/// A name the user picked for something new, possibly with folders (`Scan/sub/a.pdf`).
fn new_rel_name(s: &str) -> Option<RelPath> {
    let rel = RelPath::parse(s).ok()?;
    (!rel.is_root() && rel.components().all(|c| validate_new_name(c).is_ok())).then_some(rel)
}

fn clash_json(ctx: &Ctx, ex: &Existing, new_size: Option<u64>, can_replace: bool) -> Response {
    let body = json!({
        "error": "exists",
        "message": ctx.tf("ops.exists", &[("name", &ex.name)]),
        "clash": {
            "name": ex.name,
            "is_dir": ex.is_dir,
            "size": ex.size,
            "mtime": ex.mtime,
            "existing_text": if ex.is_dir {
                ctx.t("upload.clash_existing_folder").to_string()
            } else {
                ctx.tf("upload.clash_existing", &[
                    ("size", &ctx.size(ex.size)),
                    ("modified", &ctx.datetime(ex.mtime)),
                ])
            },
            "new_text": new_size.map(|s| ctx.tf("upload.clash_new", &[("size", &ctx.size(s))])),
            "can_replace": can_replace && !ex.is_dir,
        }
    });
    (StatusCode::CONFLICT, Json(body)).into_response()
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    tokio::task::spawn_blocking(f).await.ok()
}

#[derive(Deserialize)]
pub struct NewReq {
    vol: String,
    #[serde(default)]
    dir: String,
    name: String,
    size: u64,
    /// Unix milliseconds.
    #[serde(default)]
    mtime: Option<i64>,
    #[serde(default)]
    clash: Option<String>,
    #[serde(default)]
    batch: Option<String>,
}

/// Latest modification time accepted (end of year 9999), in milliseconds.
const MAX_MTIME_MS: i64 = 253_402_300_799_999;

pub async fn create(
    State(st): State<Shared>,
    Extension(client): Extension<ClientInfo>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    let cu = match api_user(&st, &ctx, &current, &headers, true) {
        Ok(cu) => cu,
        Err(r) => return *r,
    };
    let Ok(req) = serde_json::from_slice::<NewReq>(&body) else {
        return invalid(&ctx);
    };
    let Some(vol) = st.volumes.get(&req.vol).cloned() else {
        return op_error(&ctx, &OpError::NotFound, &req.vol);
    };
    let (Ok(dir), Some(name)) = (RelPath::parse(&req.dir), new_rel_name(&req.name)) else {
        return invalid(&ctx);
    };
    let Ok(target) = dir.join_path(&name) else {
        return invalid(&ctx);
    };
    let Some(clash) = Clash::parse(req.clash.as_deref().unwrap_or("ask")) else {
        return invalid(&ctx);
    };
    let dir_perms = path_perms(&st.cfg, &principal, client.origin, vol.name(), &dir);
    if !dir_perms.contains(Perms::READ) {
        return op_error(&ctx, &OpError::NotFound, &req.dir);
    }
    let perms = path_perms(&st.cfg, &principal, client.origin, vol.name(), &target);
    if !perms.contains(Perms::WRITE) {
        return forbidden(&ctx);
    }
    let can_replace = perms.contains(Perms::DELETE);
    if clash == Clash::Replace && !can_replace {
        return forbidden(&ctx);
    }
    let owner = actor(&cu);
    let size = req.size;
    let mtime_ms = req
        .mtime
        .unwrap_or_else(|| jiff::Timestamp::now().as_millisecond())
        .clamp(0, MAX_MTIME_MS);
    let chunk = u64::from(st.cfg.uploads.chunk_mib) << 20;
    let result = {
        let (st, owner, batch_id) = (st.clone(), owner.clone(), req.batch.clone());
        let (vol, dir, name) = (vol.clone(), dir.clone(), name.clone());
        blocking(move || -> Result<Created, OpError> {
            let batch = match batch_id {
                Some(id) => {
                    let b = st.uploads.batch(&id, &owner)?;
                    if b.volume != vol.name() || b.dir != dir.as_str() {
                        return Err(OpError::Invalid("batch"));
                    }
                    Some(b)
                }
                None => None,
            };
            st.uploads.create(
                &vol,
                &NewUpload {
                    owner: &owner,
                    dir: &dir,
                    name: &name,
                    size,
                    mtime_ms,
                    clash,
                    batch: batch.as_ref(),
                    chunk,
                },
                now(),
            )
        })
        .await
    };
    match result {
        Some(Ok(Created::Started(s))) => (StatusCode::CREATED, Json(s)).into_response(),
        Some(Ok(Created::Skipped)) => Json(json!({ "skipped": true })).into_response(),
        Some(Ok(Created::Clash(ex))) => clash_json(&ctx, &ex, Some(size), can_replace),
        Some(Err(e)) => op_error(&ctx, &e, name.as_str()),
        None => internal(&ctx),
    }
}

pub async fn status(
    State(st): State<Shared>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    let cu = match api_user(&st, &ctx, &current, &headers, false) {
        Ok(cu) => cu,
        Err(r) => return *r,
    };
    let owner = actor(&cu);
    let st2 = st.clone();
    match blocking(move || st2.uploads.status(&st2.volumes, &id, &owner)).await {
        Some(Ok(s)) => Json(s).into_response(),
        Some(Err(e)) => op_error(&ctx, &e, ""),
        None => internal(&ctx),
    }
}

pub async fn chunk(
    State(st): State<Shared>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
    Path((id, n)): Path<(String, u32)>,
    body: Body,
) -> Response {
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    let cu = match api_user(&st, &ctx, &current, &headers, true) {
        Ok(cu) => cu,
        Err(r) => return *r,
    };
    let len = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    let owner = actor(&cu);
    let ticket = {
        let st = st.clone();
        blocking(move || st.uploads.begin_chunk(&st.volumes, &id, &owner, n, len)).await
    };
    let ticket = match ticket {
        Some(Ok(t)) => Arc::new(t),
        Some(Err(e)) => return op_error(&ctx, &e, ""),
        None => return internal(&ctx),
    };

    let mut stream = body.into_data_stream();
    let mut buf: Vec<u8> = Vec::with_capacity((ticket.len as usize).min(WRITE_BUF));
    let mut written: u64 = 0;
    loop {
        let next = stream.next().await;
        let flush = match &next {
            Some(Ok(data)) => {
                if written + (buf.len() + data.len()) as u64 > ticket.len {
                    return invalid(&ctx);
                }
                buf.extend_from_slice(data);
                buf.len() >= WRITE_BUF
            }
            // The client went away or sent garbage; the ticket's drop releases the chunk.
            Some(Err(_)) => return invalid(&ctx),
            None => !buf.is_empty(),
        };
        if flush {
            let (t, data, pos) = (ticket.clone(), std::mem::take(&mut buf), written);
            match blocking(move || t.write_at(&data, pos).map(|()| data)).await {
                Some(Ok(mut data)) => {
                    written += data.len() as u64;
                    data.clear();
                    buf = data;
                }
                Some(Err(e)) => return op_error(&ctx, &OpError::from(e), ""),
                None => return internal(&ctx),
            }
        }
        if next.is_none() {
            break;
        }
    }
    if written != ticket.len {
        return invalid(&ctx);
    }
    let Ok(ticket) = Arc::try_unwrap(ticket) else {
        return internal(&ctx);
    };
    match blocking(move || ticket.complete(now())).await {
        Some(Ok(count)) => Json(json!({ "have": count })).into_response(),
        Some(Err(e)) => op_error(&ctx, &e, ""),
        None => internal(&ctx),
    }
}

#[derive(Deserialize, Default)]
struct DoneReq {
    #[serde(default)]
    clash: Option<String>,
}

pub async fn done(
    State(st): State<Shared>,
    Extension(client): Extension<ClientInfo>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Bytes,
) -> Response {
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    let cu = match api_user(&st, &ctx, &current, &headers, true) {
        Ok(cu) => cu,
        Err(r) => return *r,
    };
    let req: DoneReq = if body.is_empty() {
        DoneReq::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(r) => r,
            Err(_) => return invalid(&ctx),
        }
    };
    let clash = match req.clash.as_deref().map(Clash::parse) {
        Some(None) => return invalid(&ctx),
        Some(Some(c)) => Some(c),
        None => None,
    };
    let owner = actor(&cu);
    let rec = {
        let (st, id, owner) = (st.clone(), id.clone(), owner.clone());
        blocking(move || st.uploads.record(&st.volumes, &id, &owner)).await
    };
    let rec = match rec {
        Some(Ok(r)) => r,
        Some(Err(e)) => return op_error(&ctx, &e, ""),
        None => return internal(&ctx),
    };
    // Rights may have changed since the upload started.
    let target = RelPath::parse(&rec.dir)
        .and_then(|d| d.join_path(&RelPath::parse(&rec.name)?))
        .unwrap_or_default();
    let perms = path_perms(&st.cfg, &principal, client.origin, &rec.volume, &target);
    if !perms.contains(Perms::WRITE) {
        return forbidden(&ctx);
    }
    let can_replace = perms.contains(Perms::DELETE);
    if clash == Some(Clash::Replace) && !can_replace {
        return forbidden(&ctx);
    }
    let result = {
        let (st, owner) = (st.clone(), owner.clone());
        blocking(move || {
            st.uploads
                .finish(&st.volumes, &id, &owner, clash, can_replace, now())
        })
        .await
    };
    match result {
        Some(Ok(Finished::Done { path, renamed })) => {
            let full = RelPath::parse(&rec.dir)
                .and_then(|d| d.join_path(&path))
                .unwrap_or_default();
            audit(
                &st,
                owner,
                client.ip,
                "upload",
                &format!("{}/{}", rec.volume, full.as_str()),
                &format!(
                    "{} bytes{}",
                    rec.size,
                    if rec.staged { ", staged" } else { "" }
                ),
            );
            Json(json!({ "path": path.as_str(), "renamed": renamed })).into_response()
        }
        Some(Ok(Finished::Skipped)) => Json(json!({ "skipped": true })).into_response(),
        Some(Ok(Finished::Clash(ex))) => clash_json(&ctx, &ex, Some(rec.size), can_replace),
        Some(Err(e)) => op_error(&ctx, &e, &rec.name),
        None => internal(&ctx),
    }
}

pub async fn cancel(
    State(st): State<Shared>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    let cu = match api_user(&st, &ctx, &current, &headers, true) {
        Ok(cu) => cu,
        Err(r) => return *r,
    };
    let owner = actor(&cu);
    let st2 = st.clone();
    match blocking(move || st2.uploads.cancel(&st2.volumes, &id, &owner)).await {
        Some(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Some(Err(e)) => op_error(&ctx, &e, ""),
        None => internal(&ctx),
    }
}

#[derive(Deserialize)]
pub struct BatchReq {
    vol: String,
    #[serde(default)]
    dir: String,
}

pub async fn batch_new(
    State(st): State<Shared>,
    Extension(client): Extension<ClientInfo>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    let cu = match api_user(&st, &ctx, &current, &headers, true) {
        Ok(cu) => cu,
        Err(r) => return *r,
    };
    let Ok(req) = serde_json::from_slice::<BatchReq>(&body) else {
        return invalid(&ctx);
    };
    let (Some(vol), Ok(dir)) = (st.volumes.get(&req.vol).cloned(), RelPath::parse(&req.dir)) else {
        return op_error(&ctx, &OpError::NotFound, &req.dir);
    };
    let perms = path_perms(&st.cfg, &principal, client.origin, vol.name(), &dir);
    if !perms.contains(Perms::READ) {
        return op_error(&ctx, &OpError::NotFound, &req.dir);
    }
    if !perms.contains(Perms::WRITE) {
        return forbidden(&ctx);
    }
    let owner = actor(&cu);
    let st2 = st.clone();
    let made = blocking(move || st2.uploads.create_batch(&vol, &dir, &owner, now())).await;
    match made {
        Some(Ok(b)) => (
            StatusCode::CREATED,
            Json(json!({ "id": b.id, "staged": b.staged })),
        )
            .into_response(),
        Some(Err(e)) => op_error(&ctx, &e, &req.dir),
        None => internal(&ctx),
    }
}

pub async fn batch_done(
    State(st): State<Shared>,
    Extension(client): Extension<ClientInfo>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    let cu = match api_user(&st, &ctx, &current, &headers, true) {
        Ok(cu) => cu,
        Err(r) => return *r,
    };
    let owner = actor(&cu);
    let result = {
        let (st, owner) = (st.clone(), owner.clone());
        blocking(move || {
            let b = st.uploads.batch(&id, &owner)?;
            let moved = st.uploads.finish_batch(&st.volumes, &id, &owner)?;
            Ok::<_, OpError>((b, moved))
        })
        .await
    };
    match result {
        Some(Ok((b, moved))) => {
            for (_, used) in &moved {
                let target = RelPath::parse(&b.dir)
                    .and_then(|d| d.join(used))
                    .unwrap_or_default();
                audit(
                    &st,
                    owner.clone(),
                    client.ip,
                    "upload_folder",
                    &format!("{}/{}", b.volume, target.as_str()),
                    "",
                );
            }
            let moved: Vec<Value> = moved.into_iter().map(|(a, b)| json!([a, b])).collect();
            Json(json!({ "moved": moved })).into_response()
        }
        Some(Err(e)) => op_error(&ctx, &e, ""),
        None => internal(&ctx),
    }
}
