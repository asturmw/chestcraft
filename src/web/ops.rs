//! JSON API for rename, move and copy (dialogs, the in-app clipboard). One item per call,
//! like uploads, so the client can ask about each name clash on its own:
//!
//! ```text
//! POST /.cc/rename  {vol, path, name, clash?}
//! POST /.cc/move    {vol, path, to_vol, to_dir, clash?}
//! POST /.cc/copy    {vol, path, to_vol, to_dir, clash?}
//!   200 {path, name, renamed}      done (`renamed`: keep both picked `name (n)`)
//!   200 {skipped: true}            clash = "skip" and the name is taken
//!   409 {error: "exists", clash}   clash = "ask" (default) and the name is taken; the same
//!                                  `clash` object as for uploads, `new_text` for files
//!   4xx {error, message}           not found, forbidden, read-only area, …
//! ```
//!
//! `clash`: `ask`, `keep_both`, `replace` (the entry in the way goes to the trash; needs
//! the right to delete there) or `skip`. Signed-in accounts only, with `X-CSRF-Token`.
//! Rights as everywhere: rename and move need `m` on the item; a move needs `m` at the
//! target in the same volume, `w` in another; a copy needs `r` on the item and `w` at the
//! target. Shares follow renames and moves.

use super::api::{self, csrf_refused, invalid, json_error};
use super::auth::{audit, now};
use super::{ClientInfo, Current, Shared, page_ctx};
use crate::auth::perm::{Perms, Principal, path_perms};
use crate::fs::ops::OpError;
use crate::fs::path::{RelPath, validate_new_name};
use crate::fs::transfer::{self, CopySpec, OnClash, Placed};
use crate::fs::volume::Kind;
use axum::Json;
use axum::extract::{Extension, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
pub struct OpReq {
    vol: String,
    path: String,
    /// rename: the new name.
    #[serde(default)]
    name: String,
    /// move, copy: the target folder.
    #[serde(default)]
    to_vol: String,
    #[serde(default)]
    to_dir: String,
    #[serde(default)]
    clash: Option<String>,
}

#[derive(Clone, Copy, PartialEq)]
enum What {
    Rename,
    Move,
    Copy,
}

impl What {
    fn action(self) -> &'static str {
        match self {
            What::Rename => "rename",
            What::Move => "move",
            What::Copy => "copy",
        }
    }
}

pub async fn rename(
    st: State<Shared>,
    client: Extension<ClientInfo>,
    principal: Extension<Principal>,
    current: Extension<Current>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    run(What::Rename, st, client, principal, current, headers, body).await
}

pub async fn move_to(
    st: State<Shared>,
    client: Extension<ClientInfo>,
    principal: Extension<Principal>,
    current: Extension<Current>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    run(What::Move, st, client, principal, current, headers, body).await
}

pub async fn copy_to(
    st: State<Shared>,
    client: Extension<ClientInfo>,
    principal: Extension<Principal>,
    current: Extension<Current>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    run(What::Copy, st, client, principal, current, headers, body).await
}

fn not_found(ctx: &super::views::Ctx, name: &str) -> Response {
    let (status, code, msg) = api::message(ctx, &OpError::NotFound, name);
    json_error(status, code, &msg)
}

fn denied(ctx: &super::views::Ctx) -> Response {
    json_error(StatusCode::FORBIDDEN, "forbidden", ctx.t("ops.forbidden"))
}

#[allow(clippy::too_many_arguments)]
async fn run(
    what: What,
    State(st): State<Shared>,
    Extension(client): Extension<ClientInfo>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    let Some(cu) = current.as_deref() else {
        return json_error(
            StatusCode::UNAUTHORIZED,
            "signed_out",
            ctx.t("ops.signed_out"),
        );
    };
    let token = headers
        .get("x-csrf-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !super::auth::csrf_ok(&headers, &st, cu, token) {
        return csrf_refused(&ctx);
    }
    let Ok(req) = serde_json::from_slice::<OpReq>(&body) else {
        return invalid(&ctx);
    };
    let on_clash = match req.clash.as_deref().unwrap_or("ask") {
        "ask" => OnClash::Fail,
        "keep_both" => OnClash::KeepBoth,
        "replace" => OnClash::Trash,
        "skip" => OnClash::Skip,
        _ => return invalid(&ctx),
    };
    let perms = |vol: &str, rel: &RelPath| path_perms(&st.cfg, &principal, client.origin, vol, rel);

    // The item.
    let Some(svol) = st.volumes.get(&req.vol).cloned() else {
        return not_found(&ctx, &req.vol);
    };
    let Ok(src) = RelPath::parse(&req.path) else {
        return invalid(&ctx);
    };
    let Some(old_name) = src.name().map(str::to_string) else {
        return invalid(&ctx);
    };
    let sperms = perms(svol.name(), &src);
    if src.is_internal() || !sperms.contains(Perms::READ) {
        return not_found(&ctx, &old_name);
    }
    if what != What::Copy && !sperms.contains(Perms::MOVE) {
        return denied(&ctx);
    }

    // Where it goes, and under which name.
    let (dvol, dparent, name) = match what {
        What::Rename => {
            let name = req.name.trim().to_string();
            if validate_new_name(&name).is_err() {
                return invalid(&ctx);
            }
            let parent = src.parent().unwrap_or_default();
            (svol.clone(), parent, name)
        }
        What::Move | What::Copy => {
            let Some(dvol) = st.volumes.get(&req.to_vol).cloned() else {
                return not_found(&ctx, &req.to_vol);
            };
            let Ok(dir) = RelPath::parse(&req.to_dir) else {
                return invalid(&ctx);
            };
            if dir.is_internal() || !perms(dvol.name(), &dir).contains(Perms::READ) {
                return not_found(&ctx, &req.to_dir);
            }
            (dvol, dir, old_name.clone())
        }
    };
    let Ok(dst) = dparent.join(&name) else {
        return invalid(&ctx);
    };
    if dvol.name() == svol.name() && dst == src {
        return json_error(
            StatusCode::CONFLICT,
            "same",
            &ctx.tf("ops.same_place", &[("name", &name)]),
        );
    }
    if dvol.name() == svol.name() && what != What::Copy && dst.starts_with(&src) {
        return json_error(StatusCode::CONFLICT, "inside", ctx.t("ops.into_itself"));
    }
    let dperms = perms(dvol.name(), &dst);
    let needed = match what {
        What::Rename => Perms::MOVE,
        What::Move if dvol.name() == svol.name() => Perms::MOVE,
        What::Move | What::Copy => Perms::WRITE,
    };
    if !dperms.contains(needed) {
        return denied(&ctx);
    }
    let can_replace = dperms.contains(Perms::DELETE);
    if on_clash == OnClash::Trash && !can_replace {
        return denied(&ctx);
    }

    let actor = format!("user:{}", cu.key);
    let job = {
        let (db, svol, dvol, src, dparent, name, actor) = (
            st.db.clone(),
            svol.clone(),
            dvol.clone(),
            src.clone(),
            dparent.clone(),
            name.clone(),
            actor.clone(),
        );
        tokio::task::spawn_blocking(move || {
            let meta = match svol.stat(&src) {
                Ok(m) => m,
                Err(_) => return (Err(OpError::NotFound), None),
            };
            // The item's size, for "new: 3 MB" in the clash dialog.
            let size = (meta.kind == Kind::File).then_some(meta.size);
            if meta.kind == Kind::Other {
                return (Err(OpError::Invalid("not a file or folder")), size);
            }
            if !matches!(dvol.stat(&dparent), Ok(m) if m.kind == Kind::Dir) {
                return (Err(OpError::NotFound), size);
            }
            let t = now();
            let placed = if what == What::Copy {
                let spec = CopySpec {
                    include_hidden: false,
                    shallow: false,
                };
                transfer::copy_entry(
                    &db, &svol, &src, &dvol, &dparent, &name, spec, on_clash, &actor, t,
                )
            } else {
                transfer::move_entry(
                    &db, &svol, &src, &dvol, &dparent, &name, on_clash, &actor, t,
                )
            };
            (placed, size)
        })
        .await
    };
    match job {
        Ok((Ok(Placed::Done { name: used, .. }), _)) => {
            let path = dparent
                .join(&used)
                .map(|p| p.as_str().to_string())
                .unwrap_or_default();
            audit(
                &st,
                actor,
                client.ip,
                what.action(),
                &format!("{}/{}", svol.name(), src.as_str()),
                &format!("{}/{path}", dvol.name()),
            );
            Json(json!({ "path": path, "name": used, "renamed": used != name })).into_response()
        }
        Ok((Ok(Placed::Skipped), _)) => Json(json!({ "skipped": true })).into_response(),
        Ok((Err(OpError::Exists(ex)), size)) => api::clash_json(&ctx, &ex, size, can_replace),
        Ok((Err(e), _)) => {
            let (status, code, msg) = api::message(&ctx, &e, &old_name);
            json_error(status, code, &msg)
        }
        Err(_) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed",
            ctx.t("ops.failed"),
        ),
    }
}
