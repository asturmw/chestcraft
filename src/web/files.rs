//! Volume paths: the landing page, folder listings, file downloads (with Range) and zip
//! downloads.

use super::headers::SANDBOX_CSP;
use super::limits::DownloadGuard;
use super::query::Query;
use super::views::{self, Crumb, Ctx, ListingView, Row, VolumeCard};
use super::{AppState, ClientInfo, Current, Shared, page_ctx, stream};
use crate::auth::perm::{CHANGES, Perms, Principal, path_perms, volume_perms};
use crate::config::Note;
use crate::fs::path::{RelPath, is_internal_name, validate_existing_name};
use crate::fs::sort::natural_cmp;
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
enum Fail {
    NotFound,
    BadRequest,
    TooMany,
    Method,
    Internal,
}

fn fail(ctx: &Ctx, f: Fail) -> Response {
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
                .insert(header::ALLOW, HeaderValue::from_static("GET, HEAD"));
        }
        _ => {}
    }
    res
}

fn note_for(note: &Option<Note>, lang_code: &str) -> Option<String> {
    let n = note.as_ref()?;
    if lang_code == "de" {
        n.de.clone().or_else(|| n.en.clone())
    } else {
        n.en.clone().or_else(|| n.de.clone())
    }
}

fn enc(s: &str) -> String {
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
            note: note_for(&v.cfg.note, ctx.code()),
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
    views::landing(&ctx, &cards).into_response()
}

/// Everything below `/<Volume>/`.
pub async fn handle(
    State(st): State<Shared>,
    Extension(client): Extension<ClientInfo>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    if method != Method::GET && method != Method::HEAD {
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

    let perms = path_perms(&st.cfg, &principal, client.origin, vol.name(), &rel);
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
                return match zip_response(&st, vol, rel, &q, &principal, head, guard) {
                    Ok(r) => r,
                    Err(f) => fail(&ctx, f),
                };
            }
            listing(&ctx, vol, rel, perms, &q, &principal, raw_path).await
        }
        Kind::File => {
            if raw_path.ends_with('/') {
                return Redirect::permanent(&format!(
                    "{}{query_suffix}",
                    raw_path.trim_end_matches('/')
                ))
                .into_response();
            }
            let guard = match anon_guard(&st, &principal, &client) {
                Ok(g) => g,
                Err(()) => return fail(&ctx, Fail::TooMany),
            };
            let name = rel.name().unwrap_or("download").to_string();
            download(fd, &meta, &name, &headers, head, q.has("dl"), guard).await
        }
        Kind::Other => fail(&ctx, Fail::NotFound),
    }
}

/// Anonymous visitors get a limited number of parallel downloads per IP.
fn anon_guard(
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

async fn listing(
    ctx: &Ctx<'_>,
    vol: Arc<VolumeRoot>,
    rel: RelPath,
    perms: Perms,
    q: &Query,
    principal: &Principal,
    raw_path: &str,
) -> Response {
    let admin = principal.is_admin();
    let show_hidden = admin && q.get("hidden") == Some("1");
    let listed = {
        let (vol, rel) = (vol.clone(), rel.clone());
        tokio::task::spawn_blocking(move || -> io::Result<(Vec<Entry>, Option<Space>)> {
            let entries = vol.list(&rel, show_hidden)?;
            Ok((entries, vol.space().ok()))
        })
        .await
    };
    let (mut entries, space) = match listed {
        Ok(Ok(x)) => x,
        Ok(Err(e)) if e.kind() == io::ErrorKind::NotFound => return fail(ctx, Fail::NotFound),
        _ => return fail(ctx, Fail::Internal),
    };

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
    let pages = total.div_ceil(PAGE_SIZE).max(1);
    let page = q
        .get("page")
        .and_then(|p| p.parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, pages);
    let slice = &entries[(page - 1) * PAGE_SIZE..(page * PAGE_SIZE).min(total)];
    let rows = slice
        .iter()
        .map(|e| {
            let is_dir = e.meta.kind == Kind::Dir;
            Row {
                name: e.name.clone(),
                href: if is_dir {
                    format!("{}/", enc(&e.name))
                } else {
                    enc(&e.name)
                },
                is_dir,
                size: if is_dir {
                    String::new()
                } else {
                    ctx.size(e.meta.size)
                },
                modified: ctx.datetime(e.meta.mtime),
            }
        })
        .collect();

    // Links keep the current view settings.
    let link = |page: usize, sort: SortBy, desc: bool, hidden: bool| -> String {
        let mut params = Vec::new();
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
    let sort_link = |s: SortBy| link(1, s, s == sort && !desc, show_hidden);

    let vol_href = format!("/{}/", enc(vol.name()));
    let mut crumbs = vec![Crumb {
        label: vol.name().to_string(),
        href: vol_href.clone(),
    }];
    let mut acc = vol_href;
    for c in rel.components() {
        acc.push_str(&enc(c));
        acc.push('/');
        crumbs.push(Crumb {
            label: c.to_string(),
            href: acc.clone(),
        });
    }

    let view = ListingView {
        title: rel.name().unwrap_or(vol.name()).to_string(),
        crumbs,
        note: note_for(&vol.cfg.note, ctx.code()),
        free: space.map(|s| ctx.tf("browser.free_space", &[("free", &ctx.size(s.avail))])),
        read_only: !perms.intersects(CHANGES),
        rows,
        total,
        page,
        pages,
        prev_href: (page > 1).then(|| link(page - 1, sort, desc, show_hidden)),
        next_href: (page < pages).then(|| link(page + 1, sort, desc, show_hidden)),
        sort_hrefs: [
            sort_link(SortBy::Name),
            sort_link(SortBy::Size),
            sort_link(SortBy::Modified),
        ],
        zip_href: if show_hidden {
            format!("{raw_path}?zip&hidden=1")
        } else {
            format!("{raw_path}?zip")
        },
        hidden_toggle: admin.then(|| {
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
    };
    views::listing(ctx, &view).into_response()
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

async fn download(
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

fn zip_response(
    st: &AppState,
    vol: Arc<VolumeRoot>,
    rel: RelPath,
    q: &Query,
    principal: &Principal,
    head: bool,
    guard: Option<DownloadGuard>,
) -> Result<Response, Fail> {
    let include_hidden = principal.is_admin() && q.get("hidden") == Some("1");
    let only: Vec<String> = q.all("n").map(String::from).collect();
    for n in &only {
        if validate_existing_name(n).is_err()
            || is_internal_name(n)
            || (!include_hidden && n.starts_with('.'))
        {
            return Err(Fail::BadRequest);
        }
    }
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
