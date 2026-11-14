//! Share links for visitors. `/s/<id>` asks for the password (nothing about the content is
//! shown before), then serves the shared folder (browse, download, zip; uploads in mode
//! `view_upload`), the shared file, or the upload page of mode `upload`. The upload API for
//! visitors lives below the share, so the share's cookie reaches it: `/s/<id>/.cc/upload…`
//! and `/s/<id>/.cc/batch…`, with `dir` relative to the share. The password form posts to
//! `/s/<id>/.cc/unlock`. Downloads that save file by file use `?tree` / `op=tree` and
//! `/s/<id>/.cc/dl-slots` (see `web::files`).
//!
//! Share pages follow the system theme and the browser's language, with a language switch
//! and no sign-in link (the people a link goes to have no account). A folder's page shows
//! the share's name once (the page title; crumbs only below the top), one line with who
//! shared it and until when, the creator's note, and "Download all" (the folder's zip, a
//! plain link) as its primary button; the list/grid choice is a cookie (`web::layout`).

use super::api::{self, NewReq, Uploader};
use super::auth::same_origin;
use super::files::{self, Fail, Frame, download_guard, enc, fail};
use super::guest::{self, Opened, ShareGuest, Unlock};
use super::layout;
use super::preview::{self, ViewerReq};
use super::query::Query;
use super::share_views::{self, FileCard};
use super::views::{Ctx, ShareHead, UploadUi};
use super::{AppState, ClientInfo, Shared, page_ctx};
use crate::auth::perm::{Perms, Principal};
use crate::db::Share;
use crate::fs::path::RelPath;
use crate::fs::volume::{Kind, Meta, VolumeRoot};
use crate::shares::{self, Mode, Status};
use axum::body::Body;
use axum::extract::{Extension, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Redirect, Response};
use std::io;
use std::os::fd::OwnedFd;

/// Largest form body (the password form, a zip selection).
const MAX_FORM: usize = 1 << 20;

fn unavailable(ctx: &Ctx, which: &str) -> Response {
    let status = match which {
        "expired" | "revoked" => StatusCode::GONE,
        "paused" => StatusCode::FORBIDDEN,
        _ => StatusCode::NOT_FOUND,
    };
    (status, share_views::share_unavailable(ctx, which)).into_response()
}

fn slowed(ctx: &Ctx, wait: i64) -> String {
    let minutes = ((wait + 59) / 60).max(1);
    ctx.tf("share.slowed_down", &[("minutes", &minutes.to_string())])
}

fn head_of(ctx: &Ctx, opened: &Opened) -> ShareHead {
    let s = &opened.share;
    let path = RelPath::parse(&s.path).unwrap_or_default();
    ShareHead {
        name: path.name().unwrap_or(&s.volume).to_string(),
        by: opened
            .owner
            .as_ref()
            .map(|u| u.name.clone())
            .unwrap_or_default(),
        note: (!s.note.is_empty()).then(|| s.note.clone()),
        until: s.expires.map(|t| ctx.datetime(t)),
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn handle(
    State(st): State<Shared>,
    Extension(client): Extension<ClientInfo>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let mut ctx = page_ctx(&st, &headers, &Principal::Anonymous, None);
    ctx.theme = None;
    ctx.lang_switch = Some(uri.to_string());
    ctx.sign_in = false;

    let rest = uri.path().strip_prefix("/s/").unwrap_or_default();
    let (id, tail) = match rest.split_once('/') {
        Some((i, t)) => (i, Some(t)),
        None => (rest, None),
    };
    let base = format!("/s/{id}");
    let opened = match guest::open_share(&st, id).await {
        Ok(Some(o)) => o,
        Ok(None) => return unavailable(&ctx, "not_found"),
        Err(()) => return fail(&ctx, Fail::Internal),
    };
    if opened.status != Status::Active {
        return unavailable(&ctx, opened.status.code());
    }
    let visitor = guest::share_guest(&st, &headers, &opened).await;

    if let Some(call) = tail.and_then(|t| t.strip_prefix(".cc/")) {
        if call == "unlock" {
            if method != Method::POST {
                return Redirect::to(&base).into_response();
            }
            return unlock(&st, &ctx, &client, &headers, &opened, &base, body).await;
        }
        return internal(&st, &ctx, &client, &method, &headers, body, visitor, call).await;
    }
    if !matches!(method, Method::GET | Method::HEAD | Method::POST) {
        return fail(&ctx, Fail::Method);
    }
    let Some(visitor) = visitor else {
        let action = format!("{base}/.cc/unlock");
        return share_views::share_password(&ctx, &action, None).into_response();
    };
    content(
        &st, ctx, &client, &method, &uri, &headers, body, &opened, visitor, &base, tail,
    )
    .await
}

async fn unlock(
    st: &Shared,
    ctx: &Ctx<'_>,
    client: &ClientInfo,
    headers: &HeaderMap,
    opened: &Opened,
    base: &str,
    body: Body,
) -> Response {
    let action = format!("{base}/.cc/unlock");
    if !same_origin(headers, &st.cfg.server.public_url) {
        return super::auth::forbidden(ctx);
    }
    let Ok(bytes) = axum::body::to_bytes(body, MAX_FORM).await else {
        return fail(ctx, Fail::BadRequest);
    };
    let form = Query::parse(std::str::from_utf8(&bytes).ok());
    let input = form.get("password").unwrap_or_default().to_string();
    let share = &opened.share;
    let key = shares::share_scope(&share.id);
    let page = |status: StatusCode, msg: String| {
        (
            status,
            share_views::share_password(ctx, &action, Some(&msg)),
        )
            .into_response()
    };
    match guest::check_password(
        st,
        client,
        &key,
        share.password_hash.clone(),
        share.passphrase,
        input,
    )
    .await
    {
        Unlock::Ok => {}
        Unlock::Slowed { wait } => return page(StatusCode::TOO_MANY_REQUESTS, slowed(ctx, wait)),
        Unlock::Wrong { wait } => {
            guest::share_event(st, share, client, "unlock_failed", "");
            let msg = if wait > 0 {
                slowed(ctx, wait)
            } else {
                ctx.t("share.wrong").to_string()
            };
            return page(StatusCode::UNAUTHORIZED, msg);
        }
    }
    let Some(cookie) = guest::start(st, client, key, shares::SHARE_COOKIE, base).await else {
        return fail(ctx, Fail::Internal);
    };
    guest::share_event(st, share, client, "unlock", "");
    let now = super::now();
    guest::bump(st, &share.id, move |s| {
        s.opens += 1;
        s.last_opened = Some(now);
    })
    .await;
    let target = if share.is_dir && Mode::parse(&share.mode) != Some(Mode::Upload) {
        format!("{base}/")
    } else {
        base.to_string()
    };
    let mut res = Redirect::to(&target).into_response();
    if let Ok(v) = HeaderValue::from_str(&cookie) {
        res.headers_mut().insert(header::SET_COOKIE, v);
    }
    res
}

/// The upload API for visitors: the same calls as `/.cc/…`, below the share.
#[allow(clippy::too_many_arguments)]
async fn internal(
    st: &Shared,
    ctx: &Ctx<'_>,
    client: &ClientInfo,
    method: &Method,
    headers: &HeaderMap,
    body: Body,
    visitor: Option<ShareGuest>,
    call: &str,
) -> Response {
    let Some(g) = visitor else {
        return api::json_error(
            StatusCode::UNAUTHORIZED,
            "signed_out",
            ctx.t("share.session_ended"),
        );
    };
    let share = g.share.clone();
    let up = Uploader::Share(Box::new(g));
    if method != Method::GET && !up.change_ok(headers, st) {
        return api::csrf_refused(ctx);
    }
    let root = RelPath::parse(&share.path).unwrap_or_default();
    let read_body = |body: Body| async move { axum::body::to_bytes(body, MAX_FORM).await.ok() };
    let parts: Vec<&str> = call.split('/').collect();
    match (method.as_str(), parts.as_slice()) {
        ("POST", ["upload"]) => {
            let Some(bytes) = read_body(body).await else {
                return api::invalid(ctx);
            };
            let Ok(req) = serde_json::from_slice::<NewReq>(&bytes) else {
                return api::invalid(ctx);
            };
            let Some(dir) = RelPath::parse(&req.dir)
                .ok()
                .and_then(|d| root.join_path(&d).ok())
            else {
                return api::invalid(ctx);
            };
            api::create_in(st, ctx, client, &up, &share.volume, dir, req).await
        }
        ("GET", ["upload", id]) => api::status_in(st, ctx, &up, id.to_string()).await,
        ("GET", ["dl-slots"]) => files::slots_response(st, true, client),
        ("DELETE", ["upload", id]) => api::cancel_in(st, ctx, &up, id.to_string()).await,
        ("PUT", ["upload", id, "chunk", n]) => match n.parse::<u32>() {
            Ok(n) => api::chunk_in(st, ctx, &up, headers, id.to_string(), n, body).await,
            Err(_) => api::invalid(ctx),
        },
        ("POST", ["upload", id, "done"]) => {
            let Some(bytes) = read_body(body).await else {
                return api::invalid(ctx);
            };
            api::done_in(st, ctx, client, &up, id.to_string(), bytes).await
        }
        ("POST", ["batch"]) => {
            let Some(bytes) = read_body(body).await else {
                return api::invalid(ctx);
            };
            let Some(dir) = serde_json::from_slice::<api::BatchReq>(&bytes)
                .ok()
                .and_then(|r| RelPath::parse(&r.dir).ok())
                .and_then(|d| root.join_path(&d).ok())
            else {
                return api::invalid(ctx);
            };
            api::batch_new_in(st, ctx, client, &up, &share.volume, dir).await
        }
        ("POST", ["batch", id, "done"]) => {
            api::batch_done_in(st, ctx, client, &up, id.to_string()).await
        }
        _ => api::json_error(
            StatusCode::NOT_FOUND,
            "not_found",
            ctx.t("error.not_found_body"),
        ),
    }
}

fn upload_ui(st: &AppState, ctx: &Ctx, share: &Share, csrf: &str, dir: &RelPath) -> UploadUi {
    UploadUi {
        volume: format!("s:{}", share.id),
        dir: dir.as_str().to_string(),
        parallel: st.cfg.uploads.parallel,
        csrf: csrf.to_string(),
        api: format!("/s/{}/.cc", share.id),
        limit: share
            .max_file
            .map(|m| ctx.tf("upload.limit", &[("size", &ctx.size(m))])),
    }
}

/// Is this file request the start of a download (not a later range of the same one)?
fn starts_download(headers: &HeaderMap) -> bool {
    headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .is_none_or(|r| r.trim().starts_with("bytes=0-"))
}

#[allow(clippy::too_many_arguments)]
async fn content(
    st: &Shared,
    mut ctx: Ctx<'_>,
    client: &ClientInfo,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: Body,
    opened: &Opened,
    visitor: ShareGuest,
    base: &str,
    tail: Option<&str>,
) -> Response {
    let share = &opened.share;
    let Some(vol) = st.volumes.get(&share.volume).cloned() else {
        return unavailable(&ctx, "paused");
    };
    let root = RelPath::parse(&share.path).unwrap_or_default();
    let head = head_of(&ctx, opened);
    let mode = Mode::parse(&share.mode).unwrap_or(Mode::View);
    let q = Query::parse(uri.query());
    let head_req = *method == Method::HEAD;

    if mode == Mode::Upload {
        if tail.is_some_and(|t| !t.is_empty()) || *method == Method::POST {
            return fail(&ctx, Fail::NotFound);
        }
        ctx.scripts.push("upload.js");
        let ui = upload_ui(st, &ctx, share, &visitor.session.csrf, &RelPath::root());
        return share_views::share_dropbox(&ctx, &head, &ui, &visitor.session.uploads)
            .into_response();
    }

    if !share.is_dir {
        let name = root.name().unwrap_or("download").to_string();
        let wants_file = match tail {
            None | Some("") => false,
            Some(t) => match RelPath::from_url(t) {
                Ok(r) if r.as_str() == name => true,
                _ => return fail(&ctx, Fail::NotFound),
            },
        };
        let opened_file = open(&vol, &root).await;
        let (fd, meta) = match opened_file {
            Ok(x) if x.1.kind == Kind::File => x,
            Ok(_) => return unavailable(&ctx, "missing"),
            Err(f) => return fail(&ctx, f),
        };
        if !wants_file {
            let href = format!("{base}/{}", enc(&name));
            let body = preview::body_for(&name, &href, &meta, fd).await;
            let card = FileCard {
                size: ctx.size(meta.size),
                modified: ctx.datetime(meta.mtime),
                download_href: format!("{href}?dl"),
                ftype: crate::fs::filetype::icon_type(&name),
                name,
                body,
            };
            return share_views::share_file(&ctx, &head, &card).into_response();
        }
        if q.has("thumb") {
            return preview::thumb(
                st,
                client,
                true,
                headers,
                fd,
                &meta,
                &name,
                q.get("thumb"),
                q.get("v"),
            )
            .await;
        }
        if q.has("view") {
            return Redirect::to(base).into_response();
        }
        let Ok(guard) = download_guard(st, &Principal::Anonymous, client) else {
            return fail(&ctx, Fail::TooMany);
        };
        if !head_req && starts_download(headers) {
            guest::share_event(st, share, client, "download", &name);
            guest::bump(st, &share.id, |s| s.downloads += 1).await;
        }
        return files::download(fd, &meta, &name, headers, head_req, q.has("dl"), guard).await;
    }

    // A folder share.
    let Some(tail) = tail else {
        let suffix = uri.query().map(|qs| format!("?{qs}")).unwrap_or_default();
        return Redirect::permanent(&format!("{base}/{suffix}")).into_response();
    };
    let sub = match RelPath::from_url(tail) {
        Ok(r) => r,
        Err(crate::fs::path::PathError::BadEncoding) => return fail(&ctx, Fail::BadRequest),
        Err(_) => return fail(&ctx, Fail::NotFound),
    };
    let perms = shares::visitor_perms(&st.cfg, &visitor.owner, share, &sub);
    if !perms.contains(Perms::READ) {
        return fail(&ctx, Fail::NotFound);
    }
    let Ok(abs) = root.join_path(&sub) else {
        return fail(&ctx, Fail::NotFound);
    };
    let (fd, meta) = match open(&vol, &abs).await {
        Ok(x) => x,
        Err(f) => return fail(&ctx, f),
    };
    let raw_path = uri.path();
    let share_name = head.name.clone();
    match meta.kind {
        Kind::Dir => {
            drop(fd);
            if !raw_path.ends_with('/') {
                let suffix = uri.query().map(|qs| format!("?{qs}")).unwrap_or_default();
                return Redirect::permanent(&format!("{raw_path}/{suffix}")).into_response();
            }
            if *method != Method::POST && q.has("du") {
                return files::du_response(st, &ctx, vol, abs, false).await;
            }
            let (op, only) = if *method == Method::POST {
                let Ok(bytes) = axum::body::to_bytes(body, MAX_FORM).await else {
                    return fail(&ctx, Fail::BadRequest);
                };
                let form = Query::parse(std::str::from_utf8(&bytes).ok());
                let op = match form.get("op") {
                    Some("zip") => "zip",
                    Some("tree") => "tree",
                    _ => return fail(&ctx, Fail::BadRequest),
                };
                (op, form.all("n").map(String::from).collect::<Vec<_>>())
            } else if q.has("zip") {
                ("zip", q.all("n").map(String::from).collect())
            } else if q.has("tree") {
                ("tree", q.all("n").map(String::from).collect())
            } else {
                ("", Vec::new())
            };
            if op == "tree" {
                let Ok(guard) = download_guard(st, &Principal::Anonymous, client) else {
                    return fail(&ctx, Fail::TooMany);
                };
                return files::tree_response(vol, abs, only, false, head_req, guard)
                    .unwrap_or_else(|f| fail(&ctx, f));
            }
            if op == "zip" {
                let Ok(guard) = download_guard(st, &Principal::Anonymous, client) else {
                    return fail(&ctx, Fail::TooMany);
                };
                if !head_req {
                    guest::share_event(st, share, client, "zip", sub.as_str());
                    guest::bump(st, &share.id, |s| s.downloads += 1).await;
                }
                return match files::zip_response(st, vol, abs, only, false, head_req, guard) {
                    Ok(r) => r,
                    Err(f) => fail(&ctx, f),
                };
            }
            let upload = perms
                .contains(Perms::WRITE)
                .then(|| upload_ui(st, &ctx, share, &visitor.session.csrf, &sub));
            if upload.is_some() {
                ctx.scripts.push("upload.js");
            }
            // Everyone is a visitor here: the layout choice lives in this browser's cookie.
            let choice = layout::Choice::of(&q, headers, None);
            let frame = Frame {
                root: (share_name.clone(), format!("{base}/")),
                sub: sub.clone(),
                title: sub.name().unwrap_or(&share_name).to_string(),
                admin: false,
                form_csrf: None,
                can_mkdir: false,
                can_delete: false,
                upload,
                share: Some(head),
                share_volume: None,
                share_blocked: None,
                public_upload_href: None,
                trash_days: 0,
                layout: choice,
            };
            let mut res = files::listing(st, &ctx, vol, abs, perms, &q, raw_path, frame).await;
            choice.remember(st, client, None, &mut res).await;
            res
        }
        Kind::File => {
            if raw_path.ends_with('/') {
                return Redirect::permanent(raw_path.trim_end_matches('/')).into_response();
            }
            if *method == Method::POST {
                return fail(&ctx, Fail::Method);
            }
            let name = abs.name().unwrap_or("download").to_string();
            if q.has("thumb") {
                return preview::thumb(
                    st,
                    client,
                    true,
                    headers,
                    fd,
                    &meta,
                    &name,
                    q.get("thumb"),
                    q.get("v"),
                )
                .await;
            }
            if q.has("view") {
                let root = (share_name.clone(), format!("{base}/"));
                let req = ViewerReq {
                    vol,
                    crumbs: files::crumbs(&root, &sub.parent().unwrap_or_default()),
                    rel: abs,
                    meta,
                    fd,
                    href: raw_path.to_string(),
                    show_hidden: false,
                    delete_csrf: None,
                    share_href: None,
                    share: Some(head),
                    siblings: true,
                };
                return preview::viewer(&ctx, req).await;
            }
            let Ok(guard) = download_guard(st, &Principal::Anonymous, client) else {
                return fail(&ctx, Fail::TooMany);
            };
            if !head_req && starts_download(headers) {
                guest::share_event(st, share, client, "download", sub.as_str());
                guest::bump(st, &share.id, |s| s.downloads += 1).await;
            }
            files::download(fd, &meta, &name, headers, head_req, q.has("dl"), guard).await
        }
        Kind::Other => fail(&ctx, Fail::NotFound),
    }
}

async fn open(vol: &std::sync::Arc<VolumeRoot>, rel: &RelPath) -> Result<(OwnedFd, Meta), Fail> {
    let (vol, rel) = (vol.clone(), rel.clone());
    let opened = tokio::task::spawn_blocking(move || -> io::Result<(OwnedFd, Meta)> {
        let fd = vol.open_read(&rel)?;
        let meta = VolumeRoot::meta(&fd)?;
        Ok((fd, meta))
    })
    .await;
    match opened {
        Ok(Ok(x)) => Ok(x),
        Ok(Err(e)) if e.kind() == io::ErrorKind::NotFound => Err(Fail::NotFound),
        Ok(Err(e)) => {
            tracing::warn!("share open failed: {e}");
            Err(Fail::Internal)
        }
        Err(_) => Err(Fail::Internal),
    }
}
