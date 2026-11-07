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
//! Every call needs a session (an account's, or a public uploader's); changes also need the
//! `X-CSRF-Token` header. Share visitors use the same calls under `/s/<id>/.cc/…`, with
//! `dir` relative to the share (see `share.rs`). Visitors without an account never
//! overwrite: a taken name always becomes `name (1)`, and uploads above a share's or public
//! upload's size limit are refused (413 `too_large`). Errors are
//! `{error: <code>, message: <text in the user's language>}`.

use super::auth::{audit, csrf_ok, now, same_origin, token_eq};
use super::guest::{self, PublicGuest, ShareGuest};
use super::views::Ctx;
use super::{AppState, ClientInfo, Current, CurrentUser, Shared, page_ctx};
use crate::auth::perm::{Origin, Perms, Principal, path_perms, public_writer_perms};
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

pub fn json_error(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(json!({ "error": code, "message": message }))).into_response()
}

fn op_error(ctx: &Ctx, err: &OpError, name: &str) -> Response {
    let (status, code, msg) = message(ctx, err, name);
    json_error(status, code, &msg)
}

pub fn invalid(ctx: &Ctx) -> Response {
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

/// Who is uploading.
pub enum Uploader {
    User {
        cu: Arc<CurrentUser>,
        principal: Principal,
    },
    Public(PublicGuest),
    Share(Box<ShareGuest>),
}

impl Uploader {
    /// Owner of uploads and batches: only the same session may continue them.
    pub fn owner(&self) -> String {
        match self {
            Uploader::User { cu, .. } => format!("user:{}", cu.key),
            Uploader::Public(g) => format!("public:{}", &g.hash[..16]),
            Uploader::Share(g) => format!("share:{}:{}", g.share.id, &g.hash[..16]),
        }
    }

    /// Actor in the audit log.
    pub fn actor(&self) -> String {
        match self {
            Uploader::User { cu, .. } => format!("user:{}", cu.key),
            Uploader::Public(g) => format!("public:{}", g.volume),
            Uploader::Share(g) => format!("share:{}", g.share.id),
        }
    }

    fn csrf(&self) -> &str {
        match self {
            Uploader::User { cu, .. } => &cu.csrf,
            Uploader::Public(g) => &g.csrf,
            Uploader::Share(g) => &g.session.csrf,
        }
    }

    /// What this uploader may do at `rel` in `vol`.
    pub fn perms(&self, st: &AppState, origin: Origin, vol: &str, rel: &RelPath) -> Perms {
        match self {
            Uploader::User { principal, .. } => path_perms(&st.cfg, principal, origin, vol, rel),
            Uploader::Public(g) if g.volume == vol => {
                public_writer_perms(&st.cfg, origin, vol, rel)
            }
            Uploader::Public(_) => path_perms(&st.cfg, &Principal::Anonymous, origin, vol, rel),
            Uploader::Share(g) => {
                let root = RelPath::parse(&g.share.path).unwrap_or_default();
                match rel.strip_prefix(&root) {
                    Some(sub) if g.share.volume == vol => {
                        crate::shares::visitor_perms(&st.cfg, &g.owner, &g.share, &sub)
                    }
                    _ => Perms::empty(),
                }
            }
        }
    }

    /// Visitors without an account: never overwrite, never replace.
    fn is_guest(&self) -> bool {
        !matches!(self, Uploader::User { .. })
    }

    fn max_file(&self) -> Option<u64> {
        match self {
            Uploader::User { .. } => None,
            Uploader::Public(g) => g.max_file,
            Uploader::Share(g) => g.share.max_file,
        }
    }

    /// Origin and CSRF token check for a change.
    pub fn change_ok(&self, headers: &HeaderMap, st: &AppState) -> bool {
        match self {
            Uploader::User { cu, .. } => csrf_ok(headers, st, cu, ""),
            _ => {
                let token = headers
                    .get("x-csrf-token")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default();
                same_origin(headers, &st.cfg.server.public_url) && token_eq(token, self.csrf())
            }
        }
    }
}

fn signed_out(ctx: &Ctx) -> Response {
    json_error(
        StatusCode::UNAUTHORIZED,
        "signed_out",
        ctx.t("ops.signed_out"),
    )
}

pub fn csrf_refused(ctx: &Ctx) -> Response {
    json_error(StatusCode::FORBIDDEN, "csrf", ctx.t("error.forbidden_body"))
}

/// The uploader behind a call to `/.cc/upload…`: the signed-in user, else a public
/// uploader. Changes also need Origin + CSRF.
async fn uploader(
    st: &Shared,
    ctx: &Ctx<'_>,
    principal: &Principal,
    current: &Current,
    headers: &HeaderMap,
    change: bool,
) -> Result<Uploader, Box<Response>> {
    let up = match current.clone() {
        Some(cu) => Uploader::User {
            cu,
            principal: principal.clone(),
        },
        None => match guest::public_guest(st, headers).await {
            Some(g) => Uploader::Public(g),
            None => return Err(Box::new(signed_out(ctx))),
        },
    };
    if change && !up.change_ok(headers, st) {
        return Err(Box::new(csrf_refused(ctx)));
    }
    Ok(up)
}

/// A name the user picked for something new, possibly with folders (`Scan/sub/a.pdf`).
fn new_rel_name(s: &str) -> Option<RelPath> {
    let rel = RelPath::parse(s).ok()?;
    (!rel.is_root() && rel.components().all(|c| validate_new_name(c).is_ok())).then_some(rel)
}

pub(super) fn clash_json(
    ctx: &Ctx,
    ex: &Existing,
    new_size: Option<u64>,
    can_replace: bool,
) -> Response {
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
    /// Ignored for share visitors.
    #[serde(default)]
    pub vol: String,
    #[serde(default)]
    pub dir: String,
    pub name: String,
    pub size: u64,
    /// Unix milliseconds.
    #[serde(default)]
    pub mtime: Option<i64>,
    #[serde(default)]
    pub clash: Option<String>,
    #[serde(default)]
    pub batch: Option<String>,
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
    let up = match uploader(&st, &ctx, &principal, &current, &headers, true).await {
        Ok(up) => up,
        Err(r) => return *r,
    };
    let Ok(req) = serde_json::from_slice::<NewReq>(&body) else {
        return invalid(&ctx);
    };
    let Ok(dir) = RelPath::parse(&req.dir) else {
        return invalid(&ctx);
    };
    let vol = req.vol.clone();
    create_in(&st, &ctx, &client, &up, &vol, dir, req).await
}

/// Start an upload into `dir` of `vol` (already resolved from the request).
pub async fn create_in(
    st: &Shared,
    ctx: &Ctx<'_>,
    client: &ClientInfo,
    up: &Uploader,
    vol_name: &str,
    dir: RelPath,
    req: NewReq,
) -> Response {
    let Some(vol) = st.volumes.get(vol_name).cloned() else {
        return op_error(ctx, &OpError::NotFound, vol_name);
    };
    let Some(name) = new_rel_name(&req.name) else {
        return invalid(ctx);
    };
    let Ok(target) = dir.join_path(&name) else {
        return invalid(ctx);
    };
    let Some(mut clash) = Clash::parse(req.clash.as_deref().unwrap_or("ask")) else {
        return invalid(ctx);
    };
    if up.is_guest() {
        clash = Clash::Rename;
    }
    let dir_perms = up.perms(st, client.origin, vol.name(), &dir);
    if !dir_perms.intersects(Perms::READ | Perms::WRITE) {
        return op_error(ctx, &OpError::NotFound, &req.dir);
    }
    let perms = up.perms(st, client.origin, vol.name(), &target);
    if !perms.contains(Perms::WRITE) {
        return forbidden(ctx);
    }
    let can_replace = perms.contains(Perms::DELETE) && !up.is_guest();
    if clash == Clash::Replace && !can_replace {
        return forbidden(ctx);
    }
    if let Some(max) = up.max_file()
        && req.size > max
    {
        return json_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
            &ctx.tf(
                "ops.too_large",
                &[("name", name.as_str()), ("size", &ctx.size(max))],
            ),
        );
    }
    let owner = up.owner();
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
        Some(Ok(Created::Clash(ex))) => clash_json(ctx, &ex, Some(size), can_replace),
        Some(Err(e)) => op_error(ctx, &e, name.as_str()),
        None => internal(ctx),
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
    match uploader(&st, &ctx, &principal, &current, &headers, false).await {
        Ok(up) => status_in(&st, &ctx, &up, id).await,
        Err(r) => *r,
    }
}

pub async fn status_in(st: &Shared, ctx: &Ctx<'_>, up: &Uploader, id: String) -> Response {
    let owner = up.owner();
    let st2 = st.clone();
    match blocking(move || st2.uploads.status(&st2.volumes, &id, &owner)).await {
        Some(Ok(s)) => Json(s).into_response(),
        Some(Err(e)) => op_error(ctx, &e, ""),
        None => internal(ctx),
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
    match uploader(&st, &ctx, &principal, &current, &headers, true).await {
        Ok(up) => chunk_in(&st, &ctx, &up, &headers, id, n, body).await,
        Err(r) => *r,
    }
}

pub async fn chunk_in(
    st: &Shared,
    ctx: &Ctx<'_>,
    up: &Uploader,
    headers: &HeaderMap,
    id: String,
    n: u32,
    body: Body,
) -> Response {
    let len = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    let owner = up.owner();
    let ticket = {
        let st = st.clone();
        blocking(move || st.uploads.begin_chunk(&st.volumes, &id, &owner, n, len)).await
    };
    let ticket = match ticket {
        Some(Ok(t)) => Arc::new(t),
        Some(Err(e)) => return op_error(ctx, &e, ""),
        None => return internal(ctx),
    };

    let mut stream = body.into_data_stream();
    let mut buf: Vec<u8> = Vec::with_capacity((ticket.len as usize).min(WRITE_BUF));
    let mut written: u64 = 0;
    loop {
        let next = stream.next().await;
        let flush = match &next {
            Some(Ok(data)) => {
                if written + (buf.len() + data.len()) as u64 > ticket.len {
                    return invalid(ctx);
                }
                buf.extend_from_slice(data);
                buf.len() >= WRITE_BUF
            }
            // The client went away or sent garbage; the ticket's drop releases the chunk.
            Some(Err(_)) => return invalid(ctx),
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
                Some(Err(e)) => return op_error(ctx, &OpError::from(e), ""),
                None => return internal(ctx),
            }
        }
        if next.is_none() {
            break;
        }
    }
    if written != ticket.len {
        return invalid(ctx);
    }
    let Ok(ticket) = Arc::try_unwrap(ticket) else {
        return internal(ctx);
    };
    match blocking(move || ticket.complete(now())).await {
        Some(Ok(count)) => Json(json!({ "have": count })).into_response(),
        Some(Err(e)) => op_error(ctx, &e, ""),
        None => internal(ctx),
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
    match uploader(&st, &ctx, &principal, &current, &headers, true).await {
        Ok(up) => done_in(&st, &ctx, &client, &up, id, body).await,
        Err(r) => *r,
    }
}

pub async fn done_in(
    st: &Shared,
    ctx: &Ctx<'_>,
    client: &ClientInfo,
    up: &Uploader,
    id: String,
    body: Bytes,
) -> Response {
    let req: DoneReq = if body.is_empty() {
        DoneReq::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(r) => r,
            Err(_) => return invalid(ctx),
        }
    };
    let clash = match req.clash.as_deref().map(Clash::parse) {
        Some(None) => return invalid(ctx),
        Some(Some(c)) => Some(c),
        None => None,
    };
    let clash = if up.is_guest() {
        Some(Clash::Rename)
    } else {
        clash
    };
    let owner = up.owner();
    let rec = {
        let (st, id, owner) = (st.clone(), id.clone(), owner.clone());
        blocking(move || st.uploads.record(&st.volumes, &id, &owner)).await
    };
    let rec = match rec {
        Some(Ok(r)) => r,
        Some(Err(e)) => return op_error(ctx, &e, ""),
        None => return internal(ctx),
    };
    // Rights may have changed since the upload started.
    let target = RelPath::parse(&rec.dir)
        .and_then(|d| d.join_path(&RelPath::parse(&rec.name)?))
        .unwrap_or_default();
    let perms = up.perms(st, client.origin, &rec.volume, &target);
    if !perms.contains(Perms::WRITE) {
        return forbidden(ctx);
    }
    let can_replace = perms.contains(Perms::DELETE) && !up.is_guest();
    if clash == Some(Clash::Replace) && !can_replace {
        return forbidden(ctx);
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
            let detail = format!(
                "{} bytes{}",
                rec.size,
                if rec.staged { ", staged" } else { "" }
            );
            uploaded(st, client, up, &rec.volume, &full, &detail, !rec.staged).await;
            Json(json!({ "path": path.as_str(), "renamed": renamed })).into_response()
        }
        Some(Ok(Finished::Skipped)) => Json(json!({ "skipped": true })).into_response(),
        Some(Ok(Finished::Clash(ex))) => clash_json(ctx, &ex, Some(rec.size), can_replace),
        Some(Err(e)) => op_error(ctx, &e, &rec.name),
        None => internal(ctx),
    }
}

/// Audit a finished upload (or a staged folder that appeared), and for visitors note it in
/// their session and the share's log. `listed`: show it in the visitor's upload list (staged
/// files appear with their folder instead).
async fn uploaded(
    st: &Shared,
    client: &ClientInfo,
    up: &Uploader,
    volume: &str,
    full: &RelPath,
    detail: &str,
    listed: bool,
) {
    let target = format!("{volume}/{}", full.as_str());
    match up {
        Uploader::User { .. } => {
            audit(st, up.actor(), client.ip, "upload", &target, detail);
        }
        Uploader::Public(g) => {
            audit(st, up.actor(), client.ip, "public_upload", &target, detail);
            // Files of a staged folder are announced with the folder. One message per
            // volume and 10 minutes, so a folder of photos isn't hundreds of messages; the
            // audit log has every file.
            if g.notify && listed {
                tracing::info!(volume, path = %full, ip = %client.ip, "public upload");
                super::raise(
                    st,
                    &format!("public_upload:{volume}"),
                    600,
                    crate::health::alerts::Severity::Info,
                    crate::health::alerts::Msg::new("public_upload")
                        .text("volume", volume)
                        .text("path", target.clone())
                        .text("ip", client.ip.to_string()),
                );
            }
            if listed {
                guest::remember_uploads(st, &g.hash, vec![full.as_str().to_string()]).await;
            }
        }
        Uploader::Share(g) => {
            let root = RelPath::parse(&g.share.path).unwrap_or_default();
            let rel = full.strip_prefix(&root).unwrap_or_default();
            guest::share_event(st, &g.share, client, "upload", rel.as_str());
            guest::bump(st, &g.share.id, |s| s.uploads += 1).await;
            if listed {
                guest::remember_uploads(st, &g.hash, vec![rel.as_str().to_string()]).await;
            }
        }
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
    match uploader(&st, &ctx, &principal, &current, &headers, true).await {
        Ok(up) => cancel_in(&st, &ctx, &up, id).await,
        Err(r) => *r,
    }
}

pub async fn cancel_in(st: &Shared, ctx: &Ctx<'_>, up: &Uploader, id: String) -> Response {
    let owner = up.owner();
    let st2 = st.clone();
    match blocking(move || st2.uploads.cancel(&st2.volumes, &id, &owner)).await {
        Some(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Some(Err(e)) => op_error(ctx, &e, ""),
        None => internal(ctx),
    }
}

#[derive(Deserialize)]
pub struct BatchReq {
    /// Ignored for share visitors.
    #[serde(default)]
    pub vol: String,
    #[serde(default)]
    pub dir: String,
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
    let up = match uploader(&st, &ctx, &principal, &current, &headers, true).await {
        Ok(up) => up,
        Err(r) => return *r,
    };
    let Ok(req) = serde_json::from_slice::<BatchReq>(&body) else {
        return invalid(&ctx);
    };
    let Ok(dir) = RelPath::parse(&req.dir) else {
        return op_error(&ctx, &OpError::NotFound, &req.dir);
    };
    batch_new_in(&st, &ctx, &client, &up, &req.vol, dir).await
}

pub async fn batch_new_in(
    st: &Shared,
    ctx: &Ctx<'_>,
    client: &ClientInfo,
    up: &Uploader,
    vol_name: &str,
    dir: RelPath,
) -> Response {
    let Some(vol) = st.volumes.get(vol_name).cloned() else {
        return op_error(ctx, &OpError::NotFound, dir.as_str());
    };
    let perms = up.perms(st, client.origin, vol.name(), &dir);
    if !perms.intersects(Perms::READ | Perms::WRITE) {
        return op_error(ctx, &OpError::NotFound, dir.as_str());
    }
    if !perms.contains(Perms::WRITE) {
        return forbidden(ctx);
    }
    let owner = up.owner();
    let st2 = st.clone();
    let shown = dir.as_str().to_string();
    let made = blocking(move || st2.uploads.create_batch(&vol, &dir, &owner, now())).await;
    match made {
        Some(Ok(b)) => (
            StatusCode::CREATED,
            Json(json!({ "id": b.id, "staged": b.staged })),
        )
            .into_response(),
        Some(Err(e)) => op_error(ctx, &e, &shown),
        None => internal(ctx),
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
    match uploader(&st, &ctx, &principal, &current, &headers, true).await {
        Ok(up) => batch_done_in(&st, &ctx, &client, &up, id).await,
        Err(r) => *r,
    }
}

pub async fn batch_done_in(
    st: &Shared,
    ctx: &Ctx<'_>,
    client: &ClientInfo,
    up: &Uploader,
    id: String,
) -> Response {
    let owner = up.owner();
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
            let dir = RelPath::parse(&b.dir).unwrap_or_default();
            for (_, used) in &moved {
                let target = dir.join(used).unwrap_or_default();
                match up {
                    Uploader::User { .. } => audit(
                        st,
                        up.actor(),
                        client.ip,
                        "upload_folder",
                        &format!("{}/{}", b.volume, target.as_str()),
                        "",
                    ),
                    _ => uploaded(st, client, up, &b.volume, &target, "folder", true).await,
                }
            }
            let moved: Vec<Value> = moved.into_iter().map(|(a, b)| json!([a, b])).collect();
            Json(json!({ "moved": moved })).into_response()
        }
        Some(Err(e)) => op_error(ctx, &e, ""),
        None => internal(ctx),
    }
}
