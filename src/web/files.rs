//! Volume paths: the landing page, folder listings, file downloads (with Range), zip
//! downloads, and the folder forms (download selection, new folder, move to trash).
//!
//! JSON for downloads that save file by file (the download menu's "save to folder" and
//! "separately"):
//!
//! ```text
//! GET  <folder>/?tree[&hidden=1]        the whole folder, recursively
//! POST <folder>/  op=tree, n=<name>…     only the selected names (form, no CSRF: read only)
//!   200 application/x-ndjson, one line per entry, folders before their contents:
//!       {"path":"sub/a.jpg","dir":false,"size":123,"mtime":"2026-11-04T21:14:00Z"}
//!   (folders: "dir":true, "size":0; paths relative to <folder>)
//! GET  /.cc/dl-slots
//!   200 {"max":4,"free":2}   anonymous: the per-IP download limit and its free slots
//!   200 {"max":null}         signed in: no limit
//! ```
//!
//! The tree has the zip's rights, hidden-file rule (`hidden=1` for admins), internal-name
//! rule and download slot. Shares have both below the share: `/s/<id>/…?tree`, `POST
//! /s/<id>/…/ op=tree`, `/s/<id>/.cc/dl-slots` (share visitors always count as anonymous).

use super::auth::{audit, csrf_ok, forbidden, now};
use super::guest::{self, PublicGuest};
use super::headers::SANDBOX_CSP;
use super::limits::DownloadGuard;
use super::nav;
use super::preview::{self, ViewerReq};
use super::query::Query;
use super::views::{self, Crumb, Ctx, ListingView, Row, Section, ShareHead, UploadUi, VolumeCard};
use super::{AppState, ClientInfo, Current, CurrentUser, Shared, api, page_ctx, stream};
use crate::auth::perm::{CHANGES, Perms, Principal, path_perms, public_writer_perms, volume_perms};
use crate::fs::filetype;
use crate::fs::ops;
use crate::fs::path::{RelPath, is_internal_name, validate_existing_name, validate_new_name};
use crate::fs::sort::natural_cmp;
use crate::fs::thumbs::Size;
use crate::fs::trash;
use crate::fs::tree::{self, TreeSpec};
use crate::fs::volume::{Entry, Kind, Meta, Space, VolumeRoot};
use crate::fs::zipstream::{self, ZipSpec};
use axum::body::Body;
use axum::extract::{Extension, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Redirect, Response};
use futures_util::StreamExt;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use std::io::{self, SeekFrom};
use std::os::fd::OwnedFd;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

const PAGE_SIZE: usize = 2000;
/// Folder item counts in listings stop here.
const COUNT_CAP: usize = 10_000;

/// RFC 5987 attr-chars, for `filename*=`.
const ATTR_CHARS: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'!')
    .remove(b'#')
    .remove(b'$')
    .remove(b'&')
    .remove(b'+')
    .remove(b'-')
    .remove(b'.')
    .remove(b'^')
    .remove(b'_')
    .remove(b'`')
    .remove(b'|')
    .remove(b'~');

/// Characters escaped in URL path segments and query values we build.
const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// File types a browser would run or render as a document: always downloaded, sandboxed.
const ACTIVE_TYPES: &[&str] = &[
    "text/html",
    "application/xhtml+xml",
    "image/svg+xml",
    "text/xml",
    "application/xml",
];

#[derive(Debug, Clone, Copy)]
pub enum Fail {
    NotFound,
    BadRequest,
    TooMany,
    Method,
    Internal,
}

pub fn fail(ctx: &Ctx, f: Fail) -> Response {
    let (status, title, body) = match f {
        Fail::NotFound => (
            StatusCode::NOT_FOUND,
            "error.not_found_title",
            "error.not_found_body",
        ),
        Fail::BadRequest => (
            StatusCode::BAD_REQUEST,
            "error.bad_request_title",
            "error.bad_request_body",
        ),
        Fail::TooMany => (
            StatusCode::TOO_MANY_REQUESTS,
            "error.too_many_title",
            "error.too_many_body",
        ),
        Fail::Method => (
            StatusCode::METHOD_NOT_ALLOWED,
            "error.method_title",
            "error.method_body",
        ),
        Fail::Internal => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "error.server_error_title",
            "error.server_error_body",
        ),
    };
    let mut res = (status, views::error_page(ctx, ctx.t(title), ctx.t(body))).into_response();
    match f {
        Fail::TooMany => {
            res.headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("10"));
        }
        Fail::Method => {
            res.headers_mut()
                .insert(header::ALLOW, HeaderValue::from_static("GET, HEAD, POST"));
        }
        _ => {}
    }
    res
}

pub fn enc(s: &str) -> String {
    utf8_percent_encode(s, SEGMENT).to_string()
}

/// `/` — the volumes this visitor may read.
pub async fn landing(
    State(st): State<Shared>,
    Extension(client): Extension<ClientInfo>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
) -> Response {
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    let nav = nav::build(
        &st,
        ctx.lang,
        &principal,
        current.as_deref(),
        client.origin,
        Section::Files,
        None,
    )
    .await;
    let ctx = ctx.with_nav(nav);
    let readable: Vec<Arc<VolumeRoot>> = st
        .volumes
        .iter()
        .filter(|v| {
            volume_perms(&st.cfg, &principal, client.origin, v.name()).contains(Perms::READ)
        })
        .cloned()
        .collect();
    // statfs can block on a sick pool, so it runs off the async threads.
    let spaces: Vec<Option<Space>> = {
        let vols = readable.clone();
        tokio::task::spawn_blocking(move || vols.iter().map(|v| v.space().ok()).collect())
            .await
            .unwrap_or_default()
    };
    let cards: Vec<VolumeCard> = readable
        .iter()
        .zip(spaces.into_iter().chain(std::iter::repeat(None)))
        .map(|(v, space)| VolumeCard {
            name: v.name().to_string(),
            href: format!("/{}/", enc(v.name())),
            used: space.map(|s| s.total.saturating_sub(s.avail)),
            total: space.map(|s| s.total),
            free_text: space.map(|s| {
                ctx.tf(
                    "landing.free_of",
                    &[("free", &ctx.size(s.avail)), ("total", &ctx.size(s.total))],
                )
            }),
        })
        .collect();
    let health = principal.is_admin().then(|| views::HealthLine {
        verdict: st.health.verdict(super::now()).as_str(),
        alerts: st.health.active().len(),
    });
    views::landing(&ctx, &cards, health).into_response()
}

/// Everything below `/<Volume>/`.
#[allow(clippy::too_many_arguments)]
pub async fn handle(
    State(st): State<Shared>,
    Extension(client): Extension<ClientInfo>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    if method != Method::GET && method != Method::HEAD && method != Method::POST {
        return fail(&ctx, Fail::Method);
    }
    let head = method == Method::HEAD;
    let raw_path = uri.path();
    let q = Query::parse(uri.query());

    let trimmed = raw_path.strip_prefix('/').unwrap_or(raw_path);
    let (vol_seg, rest) = match trimmed.split_once('/') {
        Some((v, r)) => (v, r),
        None => (trimmed, ""),
    };
    let Ok(vol_name) = percent_decode_str(vol_seg).decode_utf8() else {
        return fail(&ctx, Fail::BadRequest);
    };
    let Some(vol) = st.volumes.get(&vol_name).cloned() else {
        return fail(&ctx, Fail::NotFound);
    };
    let rel = match RelPath::from_url(rest) {
        Ok(r) => r,
        Err(crate::fs::path::PathError::BadEncoding) => return fail(&ctx, Fail::BadRequest),
        Err(_) => return fail(&ctx, Fail::NotFound),
    };

    // A visitor with the public upload password of this volume.
    let guest = if principal.is_anonymous() && vol.cfg.public {
        guest::public_guest(&st, &headers)
            .await
            .filter(|g| g.volume == vol.name())
    } else {
        None
    };
    let perms = match &guest {
        Some(_) => public_writer_perms(&st.cfg, client.origin, vol.name(), &rel),
        None => path_perms(&st.cfg, &principal, client.origin, vol.name(), &rel),
    };
    if !perms.contains(Perms::READ) {
        if principal == Principal::Anonymous {
            let next = match uri.query() {
                Some(qs) => format!("{raw_path}?{qs}"),
                None => raw_path.to_string(),
            };
            return Redirect::to(&format!("/.cc/login?next={}", enc(&next))).into_response();
        }
        return fail(&ctx, Fail::NotFound);
    }

    let opened = {
        let (vol, rel) = (vol.clone(), rel.clone());
        tokio::task::spawn_blocking(move || -> io::Result<(OwnedFd, Meta)> {
            let fd = vol.open_read(&rel)?;
            let meta = VolumeRoot::meta(&fd)?;
            Ok((fd, meta))
        })
        .await
    };
    let (fd, meta) = match opened {
        Ok(Ok(x)) => x,
        Ok(Err(e)) if e.kind() == io::ErrorKind::NotFound => return fail(&ctx, Fail::NotFound),
        Ok(Err(e)) => {
            tracing::warn!(volume = %vol.name(), path = %rel, "open failed: {e}");
            return fail(&ctx, Fail::Internal);
        }
        Err(_) => return fail(&ctx, Fail::Internal),
    };

    let query_suffix = uri.query().map(|qs| format!("?{qs}")).unwrap_or_default();
    if method == Method::POST {
        drop(fd);
        if meta.kind != Kind::Dir {
            return fail(&ctx, Fail::Method);
        }
        let folder = FolderReq {
            vol,
            rel,
            perms,
            href: if raw_path.ends_with('/') {
                raw_path.to_string()
            } else {
                format!("{raw_path}/")
            },
        };
        return folder_post(
            &st,
            &ctx,
            &client,
            &principal,
            current.as_deref(),
            guest.as_ref(),
            &headers,
            folder,
            body,
        )
        .await;
    }
    match meta.kind {
        Kind::Dir => {
            drop(fd);
            if !raw_path.ends_with('/') {
                return Redirect::permanent(&format!("{raw_path}/{query_suffix}")).into_response();
            }
            if q.has("zip") {
                let guard = match anon_guard(&st, &principal, &client) {
                    Ok(g) => g,
                    Err(()) => return fail(&ctx, Fail::TooMany),
                };
                let include_hidden = principal.is_admin() && q.get("hidden") == Some("1");
                let only = q.all("n").map(String::from).collect();
                return match zip_response(&st, vol, rel, only, include_hidden, head, guard) {
                    Ok(r) => r,
                    Err(f) => fail(&ctx, f),
                };
            }
            if q.has("tree") {
                let guard = match anon_guard(&st, &principal, &client) {
                    Ok(g) => g,
                    Err(()) => return fail(&ctx, Fail::TooMany),
                };
                let include_hidden = principal.is_admin() && q.get("hidden") == Some("1");
                let only = q.all("n").map(String::from).collect();
                return match tree_response(vol, rel, only, include_hidden, head, guard) {
                    Ok(r) => r,
                    Err(f) => fail(&ctx, f),
                };
            }
            let frame = volume_frame(
                &st,
                &ctx,
                &vol,
                &rel,
                perms,
                &principal,
                guest.as_ref(),
                raw_path,
            )
            .await;
            let nav = nav::build(
                &st,
                ctx.lang,
                &principal,
                current.as_deref(),
                client.origin,
                Section::Files,
                Some(nav::Here {
                    vol: vol.clone(),
                    dir: rel.clone(),
                    page: true,
                }),
            )
            .await;
            let mut ctx = ctx.with_nav(nav);
            if frame.upload.is_some() {
                ctx.scripts.push("upload.js");
                ctx.scripts.dedup();
            }
            // Public pages follow the system theme and get a language switch.
            if principal.is_anonymous() {
                ctx.theme = None;
                ctx.lang_switch = Some(uri.to_string());
            }
            listing(&st, &ctx, vol, rel, perms, &q, raw_path, frame).await
        }
        Kind::File => {
            if raw_path.ends_with('/') {
                return Redirect::permanent(&format!(
                    "{}{query_suffix}",
                    raw_path.trim_end_matches('/')
                ))
                .into_response();
            }
            let name = rel.name().unwrap_or("download").to_string();
            if q.has("thumb") {
                return preview::thumb(
                    &st,
                    &client,
                    principal.is_anonymous(),
                    &headers,
                    fd,
                    &meta,
                    &name,
                    q.get("thumb"),
                    q.get("v"),
                )
                .await;
            }
            if q.has("view") {
                let nav = nav::build(
                    &st,
                    ctx.lang,
                    &principal,
                    current.as_deref(),
                    client.origin,
                    Section::Files,
                    Some(nav::Here {
                        vol: vol.clone(),
                        dir: rel.parent().unwrap_or_default(),
                        page: false,
                    }),
                )
                .await;
                let mut ctx = ctx.with_nav(nav);
                if principal.is_anonymous() {
                    ctx.theme = None;
                    ctx.lang_switch = Some(uri.to_string());
                }
                let root = (vol.name().to_string(), format!("/{}/", enc(vol.name())));
                let parent = rel.parent().unwrap_or_default();
                let may_share = ctx.signed_in && perms.contains(Perms::SHARE) && vol.cfg.remote;
                let req = ViewerReq {
                    crumbs: crumbs(&root, &parent),
                    share_href: may_share.then(|| {
                        format!(
                            "/.cc/shares/new?vol={}&path={}",
                            enc(vol.name()),
                            enc(rel.as_str())
                        )
                    }),
                    show_hidden: principal.is_admin() && q.get("hidden") == Some("1"),
                    delete_csrf: ctx
                        .csrf
                        .filter(|_| perms.contains(Perms::DELETE))
                        .map(str::to_string),
                    vol,
                    rel,
                    meta,
                    fd,
                    href: raw_path.to_string(),
                    share: None,
                    siblings: true,
                };
                return preview::viewer(&ctx, req).await;
            }
            let guard = match anon_guard(&st, &principal, &client) {
                Ok(g) => g,
                Err(()) => return fail(&ctx, Fail::TooMany),
            };
            download(fd, &meta, &name, &headers, head, q.has("dl"), guard).await
        }
        Kind::Other => fail(&ctx, Fail::NotFound),
    }
}

/// Anonymous visitors get a limited number of parallel downloads per IP.
pub fn anon_guard(
    st: &AppState,
    principal: &Principal,
    client: &ClientInfo,
) -> Result<Option<DownloadGuard>, ()> {
    if *principal != Principal::Anonymous {
        return Ok(None);
    }
    st.limiter
        .try_acquire(client.ip, st.cfg.limits.anon_downloads_per_ip)
        .map(Some)
        .ok_or(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortBy {
    Name,
    Size,
    Modified,
}

/// What surrounds a listing: a volume's folder browser or a share's.
pub struct Frame {
    /// First breadcrumb (label, link); the components of `sub` follow it.
    pub root: (String, String),
    pub sub: RelPath,
    pub title: String,
    /// The admin: hidden-files switch.
    pub admin: bool,
    pub form_csrf: Option<String>,
    pub can_mkdir: bool,
    pub can_delete: bool,
    pub upload: Option<UploadUi>,
    pub share: Option<ShareHead>,
    /// A signed-in user who may share here: links to create shares in this volume.
    pub share_volume: Option<String>,
    pub share_blocked: Option<String>,
    pub public_upload_href: Option<String>,
    pub trash_days: u32,
}

/// The frame of a volume folder for the visitor behind the request.
#[allow(clippy::too_many_arguments)]
async fn volume_frame(
    st: &Shared,
    ctx: &Ctx<'_>,
    vol: &VolumeRoot,
    rel: &RelPath,
    perms: Perms,
    principal: &Principal,
    guest: Option<&PublicGuest>,
    raw_path: &str,
) -> Frame {
    let form_csrf = ctx
        .csrf
        .map(str::to_string)
        .or_else(|| guest.map(|g| g.csrf.clone()));
    let may_write = form_csrf.is_some() && perms.contains(Perms::WRITE);
    let upload = may_write.then(|| UploadUi {
        volume: vol.name().to_string(),
        dir: rel.as_str().to_string(),
        parallel: st.cfg.uploads.parallel,
        csrf: form_csrf.clone().unwrap_or_default(),
        api: "/.cc".into(),
        limit: guest
            .and_then(|g| g.max_file)
            .map(|m| ctx.tf("upload.limit", &[("size", &ctx.size(m))])),
    });
    let can_share = ctx.signed_in && perms.contains(Perms::SHARE);
    let public_upload_href = if principal.is_anonymous() && guest.is_none() && vol.cfg.public {
        let (st2, name) = (st.clone(), vol.name().to_string());
        let open = tokio::task::spawn_blocking(move || {
            st2.db
                .public_write(&name)
                .ok()
                .flatten()
                .is_some_and(|pw| crate::shares::public_open(&st2.cfg, &pw, now()))
        })
        .await
        .unwrap_or(false);
        open.then(|| {
            format!(
                "/.cc/public-write?vol={}&next={}",
                enc(vol.name()),
                enc(raw_path)
            )
        })
    } else {
        None
    };
    Frame {
        root: (vol.name().to_string(), format!("/{}/", enc(vol.name()))),
        sub: rel.clone(),
        title: rel.name().unwrap_or(vol.name()).to_string(),
        admin: principal.is_admin(),
        can_mkdir: may_write,
        can_delete: ctx.csrf.is_some() && perms.contains(Perms::DELETE),
        form_csrf,
        upload,
        share: None,
        share_volume: (can_share && vol.cfg.remote).then(|| vol.name().to_string()),
        share_blocked: (can_share && !vol.cfg.remote)
            .then(|| ctx.t("browser.share_local_only").to_string()),
        public_upload_href,
        trash_days: vol.cfg.trash_days(&st.cfg),
    }
}

/// Breadcrumbs: the root (label, link), then each folder of `sub`.
pub fn crumbs(root: &(String, String), sub: &RelPath) -> Vec<Crumb> {
    let mut out = vec![Crumb {
        label: root.0.clone(),
        href: root.1.clone(),
    }];
    let mut acc = root.1.clone();
    for c in sub.components() {
        acc.push_str(&enc(c));
        acc.push('/');
        out.push(Crumb {
            label: c.to_string(),
            href: acc.clone(),
        });
    }
    out
}

#[allow(clippy::too_many_arguments)]
pub async fn listing(
    st: &AppState,
    ctx: &Ctx<'_>,
    vol: Arc<VolumeRoot>,
    rel: RelPath,
    perms: Perms,
    q: &Query,
    raw_path: &str,
    frame: Frame,
) -> Response {
    let show_hidden = frame.admin && q.get("hidden") == Some("1");
    let badges = ctx.signed_in;
    let listed = {
        let (vol, rel, db) = (vol.clone(), rel.clone(), st.db.clone());
        tokio::task::spawn_blocking(move || -> io::Result<(Vec<Entry>, Vec<crate::db::Share>)> {
            let entries = vol.list(&rel, show_hidden)?;
            let shares = if badges {
                db.shares().unwrap_or_default()
            } else {
                Vec::new()
            };
            Ok((entries, shares))
        })
        .await
    };
    let (mut entries, shares) = match listed {
        Ok(Ok(x)) => x,
        Ok(Err(e)) if e.kind() == io::ErrorKind::NotFound => return fail(ctx, Fail::NotFound),
        _ => return fail(ctx, Fail::Internal),
    };
    let shared = crate::shares::shared_names(&shares, vol.name(), &rel, now());

    let sort = match q.get("sort") {
        Some("size") => SortBy::Size,
        Some("modified") => SortBy::Modified,
        _ => SortBy::Name,
    };
    let desc = q.get("order") == Some("desc");
    entries.sort_by(|a, b| {
        let dirs_first = (b.meta.kind == Kind::Dir).cmp(&(a.meta.kind == Kind::Dir));
        dirs_first.then_with(|| {
            let o = match sort {
                SortBy::Name => natural_cmp(&a.name, &b.name),
                SortBy::Size => a
                    .meta
                    .size
                    .cmp(&b.meta.size)
                    .then_with(|| natural_cmp(&a.name, &b.name)),
                SortBy::Modified => a
                    .meta
                    .mtime
                    .cmp(&b.meta.mtime)
                    .then_with(|| natural_cmp(&a.name, &b.name)),
            };
            if desc { o.reverse() } else { o }
        })
    });

    let total = entries.len();
    let total_bytes: u64 = entries
        .iter()
        .filter(|e| e.meta.kind != Kind::Dir)
        .map(|e| e.meta.size)
        .sum();
    let has_files = entries.iter().any(|e| e.meta.kind != Kind::Dir);
    let pages = total.div_ceil(PAGE_SIZE).max(1);
    let page = q
        .get("page")
        .and_then(|p| p.parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, pages);
    let slice = entries[(page - 1) * PAGE_SIZE..(page * PAGE_SIZE).min(total)].to_vec();
    // How many items each folder on this page holds (the size column of folders).
    let counts: Vec<Option<usize>> = {
        let (vol, rel, names) = (
            vol.clone(),
            rel.clone(),
            slice
                .iter()
                .map(|e| (e.meta.kind == Kind::Dir).then(|| e.name.clone()))
                .collect::<Vec<_>>(),
        );
        tokio::task::spawn_blocking(move || {
            names
                .iter()
                .map(|n| {
                    let sub = rel.join(n.as_ref()?).ok()?;
                    vol.count(&sub, show_hidden, COUNT_CAP).ok()
                })
                .collect()
        })
        .await
        .unwrap_or_default()
    };
    let grid = q.get("layout") == Some("grid");
    let keep = if show_hidden { "&hidden=1" } else { "" };
    let rows = slice
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let is_dir = e.meta.kind == Kind::Dir;
            let name = enc(&e.name);
            let share_href = frame.share_volume.as_ref().and_then(|v| {
                let path = rel.join(&e.name).ok()?;
                Some(format!(
                    "/.cc/shares/new?vol={}&path={}",
                    enc(v),
                    enc(path.as_str())
                ))
            });
            let size = if is_dir {
                counts
                    .get(i)
                    .copied()
                    .flatten()
                    .map(|n| items_label(ctx, n))
                    .unwrap_or_default()
            } else {
                ctx.size(e.meta.size)
            };
            let date = ctx.date(e.meta.mtime);
            Row {
                name: e.name.clone(),
                href: if is_dir {
                    format!("{name}/")
                } else {
                    format!("{name}?view{keep}")
                },
                ftype: if is_dir {
                    "folder"
                } else {
                    filetype::icon_type(&e.name)
                },
                thumb: (!is_dir && filetype::thumb_source(&e.name).is_some())
                    .then(|| format!("{name}{}", preview::thumb_query(&e.meta, Size::Grid))),
                download_href: (!is_dir).then(|| format!("{name}?dl")),
                is_dir,
                bytes: if is_dir { 0 } else { e.meta.size },
                meta: if size.is_empty() {
                    date
                } else {
                    ctx.tf("browser.meta", &[("size", &size), ("date", &date)])
                },
                size,
                modified: ctx.datetime(e.meta.mtime),
                modified_iso: ctx.iso_local(e.meta.mtime),
                shared: shared.get(&e.name).map(|until| match until {
                    Some(t) => ctx.tf("browser.shared_until", &[("date", &ctx.date(*t))]),
                    None => ctx.t("browser.shared_forever").to_string(),
                }),
                share_href,
            }
        })
        .collect();

    // Links keep the current view settings.
    let link_in = |grid: bool, page: usize, sort: SortBy, desc: bool, hidden: bool| -> String {
        let mut params = Vec::new();
        if grid {
            params.push("layout=grid".to_string());
        }
        match sort {
            SortBy::Name => {}
            SortBy::Size => params.push("sort=size".to_string()),
            SortBy::Modified => params.push("sort=modified".to_string()),
        }
        if desc {
            params.push("order=desc".into());
        }
        if hidden {
            params.push("hidden=1".into());
        }
        if page > 1 {
            params.push(format!("page={page}"));
        }
        if params.is_empty() {
            raw_path.to_string()
        } else {
            format!("{raw_path}?{}", params.join("&"))
        }
    };
    let link = |page: usize, sort: SortBy, desc: bool, hidden: bool| {
        link_in(grid, page, sort, desc, hidden)
    };
    let sort_link = |s: SortBy| link(1, s, s == sort && !desc, show_hidden);

    let crumbs = crumbs(&frame.root, &frame.sub);
    let items = items_label(ctx, total);
    let summary = if has_files {
        ctx.tf(
            "browser.summary",
            &[("items", &items), ("size", &ctx.size(total_bytes))],
        )
    } else {
        items
    };
    let (sort_col, column_key) = match sort {
        SortBy::Name => (0, "browser.sorted_name"),
        SortBy::Size => (1, "browser.sorted_size"),
        SortBy::Modified => (2, "browser.sorted_modified"),
    };
    let caption = ctx.tf(
        if desc {
            "browser.caption_desc"
        } else {
            "browser.caption"
        },
        &[("name", &frame.title), ("column", ctx.t(column_key))],
    );
    let view = ListingView {
        title: frame.title,
        crumbs,
        summary,
        caption,
        read_only: !perms.intersects(CHANGES),
        rows,
        page,
        pages,
        prev_href: (page > 1).then(|| link(page - 1, sort, desc, show_hidden)),
        next_href: (page < pages).then(|| link(page + 1, sort, desc, show_hidden)),
        sort_hrefs: [
            sort_link(SortBy::Name),
            sort_link(SortBy::Size),
            sort_link(SortBy::Modified),
        ],
        sorted: (sort_col, desc),
        folder_href: raw_path.to_string(),
        post_href: raw_path.to_string(),
        form_csrf: frame.form_csrf,
        can_write: frame.can_mkdir,
        can_delete: frame.can_delete,
        upload: frame.upload,
        trash_days: frame.trash_days,
        hidden_toggle: frame.admin.then(|| {
            if show_hidden {
                (
                    ctx.t("browser.hide_hidden").to_string(),
                    link(1, sort, desc, false),
                )
            } else {
                (
                    ctx.t("browser.show_hidden").to_string(),
                    link(1, sort, desc, true),
                )
            }
        }),
        showing_hidden: show_hidden,
        share: frame.share,
        share_base: frame
            .share_volume
            .as_ref()
            .map(|v| format!("/.cc/shares/new?vol={}&path=", enc(v))),
        dir: rel.as_str().to_string(),
        share_blocked: frame.share_blocked,
        public_upload_href: frame.public_upload_href,
        layout: if grid { "grid" } else { "list" },
        layout_hrefs: [
            link_in(false, page, sort, desc, show_hidden),
            link_in(true, page, sort, desc, show_hidden),
        ],
    };
    views::listing(ctx, &view).into_response()
}

/// "6 items"; a count that reached [`COUNT_CAP`] reads "10,000+ items".
fn items_label(ctx: &Ctx, n: usize) -> String {
    if n >= COUNT_CAP {
        ctx.tf(
            "browser.items_many",
            &[("count", &ctx.number(COUNT_CAP as u64))],
        )
    } else {
        ctx.tn("browser.items", n as u64)
    }
}

enum RangeReq {
    Full,
    Part(u64, u64),
    Unsatisfiable,
}

/// One `bytes=` range; multi-range requests get the whole file (allowed by RFC 9110).
fn parse_range(value: Option<&HeaderValue>, size: u64) -> RangeReq {
    let Some(spec) = value
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().strip_prefix("bytes="))
    else {
        return RangeReq::Full;
    };
    if spec.contains(',') {
        return RangeReq::Full;
    }
    let Some((a, b)) = spec.split_once('-') else {
        return RangeReq::Full;
    };
    let (a, b) = (a.trim(), b.trim());
    match (a.is_empty(), b.is_empty()) {
        (true, false) => match b.parse::<u64>() {
            Ok(0) => RangeReq::Unsatisfiable,
            Ok(_) if size == 0 => RangeReq::Unsatisfiable,
            Ok(n) => RangeReq::Part(size.saturating_sub(n), size - 1),
            Err(_) => RangeReq::Full,
        },
        (false, true) => match a.parse::<u64>() {
            Ok(s) if s >= size => RangeReq::Unsatisfiable,
            Ok(s) => RangeReq::Part(s, size - 1),
            Err(_) => RangeReq::Full,
        },
        (false, false) => match (a.parse::<u64>(), b.parse::<u64>()) {
            (Ok(s), Ok(e)) if s > e || s >= size => RangeReq::Unsatisfiable,
            (Ok(s), Ok(e)) => RangeReq::Part(s, e.min(size - 1)),
            _ => RangeReq::Full,
        },
        (true, true) => RangeReq::Full,
    }
}

fn etag_matches(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(',')
                .any(|t| t.trim().trim_start_matches("W/") == etag || t.trim() == "*")
        })
}

fn http_date(unix: i64) -> String {
    jiff::Timestamp::from_second(unix)
        .map(|ts| ts.strftime("%a, %d %b %Y %H:%M:%S GMT").to_string())
        .unwrap_or_default()
}

pub fn content_disposition(kind: &str, name: &str) -> String {
    let fallback: String = name
        .chars()
        .map(|c| {
            if (c.is_ascii_graphic() || c == ' ') && c != '"' && c != '\\' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!(
        "{kind}; filename=\"{fallback}\"; filename*=UTF-8''{}",
        utf8_percent_encode(name, ATTR_CHARS)
    )
}

pub async fn download(
    fd: OwnedFd,
    meta: &Meta,
    name: &str,
    headers: &HeaderMap,
    head: bool,
    force_download: bool,
    guard: Option<DownloadGuard>,
) -> Response {
    let etag = meta.etag();
    let last_modified = http_date(meta.mtime);
    let mime = mime_guess::from_path(name).first_or_octet_stream();
    let active = ACTIVE_TYPES.contains(&mime.essence_str());
    let mut content_type = mime.essence_str().to_string();
    if mime.type_() == mime_guess::mime::TEXT {
        content_type.push_str("; charset=utf-8");
    }

    if etag_matches(headers, &etag) {
        return (
            StatusCode::NOT_MODIFIED,
            [(header::ETAG, etag), (header::LAST_MODIFIED, last_modified)],
        )
            .into_response();
    }

    let size = meta.size;
    let if_range_ok = headers
        .get(header::IF_RANGE)
        .is_none_or(|v| v.to_str().is_ok_and(|v| v == etag));
    let range = if if_range_ok {
        parse_range(headers.get(header::RANGE), size)
    } else {
        RangeReq::Full
    };
    let (status, start, len) = match range {
        RangeReq::Full => (StatusCode::OK, 0, size),
        RangeReq::Part(s, e) => (StatusCode::PARTIAL_CONTENT, s, e - s + 1),
        RangeReq::Unsatisfiable => {
            return (
                StatusCode::RANGE_NOT_SATISFIABLE,
                [(header::CONTENT_RANGE, format!("bytes */{size}"))],
            )
                .into_response();
        }
    };

    let disposition = content_disposition(
        if force_download || active {
            "attachment"
        } else {
            "inline"
        },
        name,
    );
    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, len)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::ETAG, &etag)
        .header(header::LAST_MODIFIED, last_modified)
        .header(header::CONTENT_DISPOSITION, disposition);
    if status == StatusCode::PARTIAL_CONTENT {
        builder = builder.header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{}/{size}", start + len - 1),
        );
    }
    if active {
        builder = builder.header(header::CONTENT_SECURITY_POLICY, SANDBOX_CSP);
    }
    if head {
        return builder
            .body(Body::empty())
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }

    let mut file = tokio::fs::File::from_std(std::fs::File::from(fd));
    if start > 0 && file.seek(SeekFrom::Start(start)).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let body = ReaderStream::with_capacity(file.take(len), 256 * 1024).map(move |chunk| {
        let _slot = &guard;
        chunk
    });
    builder
        .body(Body::from_stream(body))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// `GET /.cc/dl-slots`: free download slots of this visitor (see the module docs).
pub async fn dl_slots(
    State(st): State<Shared>,
    Extension(client): Extension<ClientInfo>,
    Extension(principal): Extension<Principal>,
) -> Response {
    slots_response(&st, principal == Principal::Anonymous, &client)
}

/// The slots answer: limited (anonymous visitors, share visitors) or not.
pub fn slots_response(st: &AppState, limited: bool, client: &ClientInfo) -> Response {
    let body = if limited {
        let max = st.cfg.limits.anon_downloads_per_ip;
        serde_json::json!({"max": max, "free": max.saturating_sub(st.limiter.active(client.ip))})
    } else {
        serde_json::json!({"max": null})
    };
    (
        [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        axum::Json(body),
    )
        .into_response()
}

/// A download selection: existing, visible names (dot-names only with `include_hidden`).
fn check_selection(only: &[String], include_hidden: bool) -> Result<(), Fail> {
    for n in only {
        if validate_existing_name(n).is_err()
            || is_internal_name(n)
            || (!include_hidden && n.starts_with('.'))
        {
            return Err(Fail::BadRequest);
        }
    }
    Ok(())
}

/// The recursive listing of a folder (or of the selected `only` names in it) as JSON
/// lines, streamed; `guard` holds the visitor's download slot until it ends.
pub fn tree_response(
    vol: Arc<VolumeRoot>,
    rel: RelPath,
    only: Vec<String>,
    include_hidden: bool,
    head: bool,
    guard: Option<DownloadGuard>,
) -> Result<Response, Fail> {
    check_selection(&only, include_hidden)?;
    let builder = Response::builder()
        .header(header::CONTENT_TYPE, "application/x-ndjson; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store");
    if head {
        return builder.body(Body::empty()).map_err(|_| Fail::Internal);
    }
    let spec = TreeSpec {
        include_hidden,
        only: (!only.is_empty()).then_some(only),
    };
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    tokio::task::spawn_blocking(move || {
        let writer = stream::ChannelWriter::new(tx.clone());
        if let Err(e) = tree::write_tree(&vol, &rel, &spec, writer)
            && e.kind() != io::ErrorKind::BrokenPipe
        {
            tracing::warn!(volume = %vol.name(), path = %rel, "tree listing failed: {e}");
            let _ = tx.blocking_send(Err(e));
        }
    });
    builder
        .body(stream::body_from_channel(rx, guard))
        .map_err(|_| Fail::Internal)
}

pub fn zip_response(
    st: &AppState,
    vol: Arc<VolumeRoot>,
    rel: RelPath,
    only: Vec<String>,
    include_hidden: bool,
    head: bool,
    guard: Option<DownloadGuard>,
) -> Result<Response, Fail> {
    check_selection(&only, include_hidden)?;
    let file_name = format!("{}.zip", rel.name().unwrap_or(vol.name()));
    let builder = Response::builder()
        .header(header::CONTENT_TYPE, "application/zip")
        .header(
            header::CONTENT_DISPOSITION,
            content_disposition("attachment", &file_name),
        );
    if head {
        return builder.body(Body::empty()).map_err(|_| Fail::Internal);
    }
    let spec = ZipSpec {
        include_hidden,
        only: (!only.is_empty()).then_some(only),
        tz: st.cfg.server.timezone.clone(),
    };
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    tokio::task::spawn_blocking(move || {
        let writer = stream::ChannelWriter::new(tx.clone());
        if let Err(e) = zipstream::write_zip(&vol, &rel, &spec, writer)
            && e.kind() != io::ErrorKind::BrokenPipe
        {
            tracing::warn!(volume = %vol.name(), path = %rel, "zip failed: {e}");
            let _ = tx.blocking_send(Err(e));
        }
    });
    builder
        .body(stream::body_from_channel(rx, guard))
        .map_err(|_| Fail::Internal)
}

/// A folder that received a form.
struct FolderReq {
    vol: Arc<VolumeRoot>,
    rel: RelPath,
    perms: Perms,
    /// The folder's URL, with a trailing slash.
    href: String,
}

/// Largest form body accepted (a selection of many names).
const MAX_FORM: usize = 1 << 20;

fn problems(ctx: &Ctx, status: StatusCode, intro: &str, items: &[String], back: &str) -> Response {
    (
        status,
        views::problems(ctx, ctx.t("ops.failed_title"), intro, items, Some(back)),
    )
        .into_response()
}

/// `POST /<Volume>/<folder>/` with `op` = `zip` (download the selected `n`), `tree` (their
/// recursive listing), `mkdir` (`name`) or `delete` (move the selected `n` to the trash).
#[allow(clippy::too_many_arguments)]
async fn folder_post(
    st: &Shared,
    ctx: &Ctx<'_>,
    client: &ClientInfo,
    principal: &Principal,
    current: Option<&CurrentUser>,
    guest: Option<&PublicGuest>,
    headers: &HeaderMap,
    folder: FolderReq,
    body: Body,
) -> Response {
    let Ok(bytes) = axum::body::to_bytes(body, MAX_FORM).await else {
        return fail(ctx, Fail::BadRequest);
    };
    let form = Query::parse(std::str::from_utf8(&bytes).ok());
    let op = form.get("op").unwrap_or_default();
    if op == "zip" || op == "tree" {
        let guard = match anon_guard(st, principal, client) {
            Ok(g) => g,
            Err(()) => return fail(ctx, Fail::TooMany),
        };
        let include_hidden = principal.is_admin() && form.get("hidden") == Some("1");
        let only = form.all("n").map(String::from).collect();
        let (vol, rel) = (folder.vol, folder.rel);
        let res = if op == "zip" {
            zip_response(st, vol, rel, only, include_hidden, false, guard)
        } else {
            tree_response(vol, rel, only, include_hidden, false, guard)
        };
        return res.unwrap_or_else(|f| fail(ctx, f));
    }
    let token = form.get("csrf").unwrap_or_default();
    // A signed-in user, or a public uploader (who may only create folders).
    let public_writer = match (current, guest) {
        (Some(cu), _) => {
            if !csrf_ok(headers, st, cu, token) {
                return forbidden(ctx);
            }
            false
        }
        (None, Some(g)) if op == "mkdir" => {
            let token = headers
                .get("x-csrf-token")
                .and_then(|v| v.to_str().ok())
                .unwrap_or(token);
            if !super::auth::same_origin(headers, &st.cfg.server.public_url)
                || !super::auth::token_eq(token, &g.csrf)
            {
                return forbidden(ctx);
            }
            true
        }
        _ => return forbidden(ctx),
    };
    let actor = match (current, guest) {
        (Some(cu), _) => format!("user:{}", cu.key),
        (None, Some(g)) => format!("public:{}", g.volume),
        (None, None) => return forbidden(ctx),
    };
    let perms_at = |r: &RelPath| {
        if public_writer {
            public_writer_perms(&st.cfg, client.origin, folder.vol.name(), r)
        } else {
            path_perms(&st.cfg, principal, client.origin, folder.vol.name(), r)
        }
    };
    match op {
        "mkdir" => {
            let name = form.get("name").unwrap_or_default().trim().to_string();
            let target = match validate_new_name(&name).map(|()| folder.rel.join(&name)) {
                Ok(Ok(t)) => t,
                _ => {
                    return problems(
                        ctx,
                        StatusCode::BAD_REQUEST,
                        ctx.t("ops.invalid"),
                        &[],
                        &folder.href,
                    );
                }
            };
            let perms = perms_at(&target);
            if !perms.contains(Perms::WRITE) {
                return problems(
                    ctx,
                    StatusCode::FORBIDDEN,
                    ctx.t("ops.forbidden"),
                    &[],
                    &folder.href,
                );
            }
            let made = {
                let (vol, rel, name) = (folder.vol.clone(), folder.rel.clone(), name.clone());
                tokio::task::spawn_blocking(move || ops::mkdir_in(vol.open_dir(&rel)?, &name)).await
            };
            match made {
                Ok(Ok(())) => {
                    audit(
                        st,
                        actor,
                        client.ip,
                        "mkdir",
                        &format!("{}/{}", folder.vol.name(), target.as_str()),
                        "",
                    );
                    Redirect::to(&folder.href).into_response()
                }
                Ok(Err(e)) => {
                    let (status, _, msg) = api::message(ctx, &e, &name);
                    problems(ctx, status, &msg, &[], &folder.href)
                }
                Err(_) => fail(ctx, Fail::Internal),
            }
        }
        "delete" => {
            if !folder.perms.contains(Perms::READ) {
                return fail(ctx, Fail::NotFound);
            }
            let mut failures = Vec::new();
            for name in form.all("n") {
                let target = match folder.rel.join(name) {
                    Ok(t) if !is_internal_name(name) => t,
                    _ => {
                        failures.push(ctx.tf("ops.not_found", &[("name", name)]));
                        continue;
                    }
                };
                let perms = perms_at(&target);
                if !perms.contains(Perms::READ) {
                    failures.push(ctx.tf("ops.not_found", &[("name", name)]));
                    continue;
                }
                if !perms.contains(Perms::DELETE) {
                    failures.push(ctx.tf("ops.no_delete", &[("name", name)]));
                    continue;
                }
                let done = {
                    let (st, vol, target, actor) = (
                        st.clone(),
                        folder.vol.clone(),
                        target.clone(),
                        actor.clone(),
                    );
                    tokio::task::spawn_blocking(move || {
                        trash::delete(&st.db, &vol, &target, &actor, trash::REASON_DELETE, now())
                    })
                    .await
                };
                match done {
                    Ok(Ok(item)) => audit(
                        st,
                        actor.clone(),
                        client.ip,
                        "trash",
                        &format!("{}/{}", item.volume, item.path),
                        &format!("{} bytes, {} files", item.size, item.files),
                    ),
                    Ok(Err(e)) => failures.push(api::message(ctx, &e, name).2),
                    Err(_) => failures.push(ctx.tf("ops.failed", &[("name", name)])),
                }
            }
            if failures.is_empty() {
                Redirect::to(&folder.href).into_response()
            } else {
                problems(
                    ctx,
                    StatusCode::CONFLICT,
                    ctx.t("ops.trash_failed"),
                    &failures,
                    &folder.href,
                )
            }
        }
        _ => fail(ctx, Fail::BadRequest),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hv(s: &str) -> HeaderValue {
        HeaderValue::from_str(s).unwrap()
    }

    #[test]
    fn ranges() {
        let r = |s: &str| match parse_range(Some(&hv(s)), 100) {
            RangeReq::Full => "full".to_string(),
            RangeReq::Part(a, b) => format!("{a}-{b}"),
            RangeReq::Unsatisfiable => "416".to_string(),
        };
        assert_eq!(r("bytes=0-9"), "0-9");
        assert_eq!(r("bytes=90-"), "90-99");
        assert_eq!(r("bytes=-10"), "90-99");
        assert_eq!(r("bytes=-1000"), "0-99");
        assert_eq!(r("bytes=50-1000"), "50-99");
        assert_eq!(r("bytes=100-"), "416");
        assert_eq!(r("bytes=9-3"), "416");
        assert_eq!(r("bytes=-0"), "416");
        assert_eq!(r("bytes=0-1,5-6"), "full");
        assert_eq!(r("items=0-1"), "full");
        assert_eq!(r("bytes=x-y"), "full");
    }

    #[test]
    fn disposition_is_ascii_safe() {
        let d = content_disposition("attachment", "Straße \"neu\".pdf");
        assert_eq!(
            d,
            "attachment; filename=\"Stra_e _neu_.pdf\"; filename*=UTF-8''Stra%C3%9Fe%20%22neu%22.pdf"
        );
        assert!(HeaderValue::from_str(&d).is_ok());
    }

    #[test]
    fn http_dates() {
        assert_eq!(http_date(0), "Thu, 01 Jan 1970 00:00:00 GMT");
    }
}
