//! WebDAV on the same URLs as the web UI, for desktop mounts (rclone, Finder, Explorer, file
//! managers): `/` lists the volumes, `/<Volume>/…` is the volume.
//!
//! Methods: OPTIONS, PROPFIND (Depth 0 and 1), PROPPATCH (modification time only), MKCOL,
//! PUT, DELETE, COPY, MOVE, and LOCK/UNLOCK (advisory: locks are granted, never enforced,
//! which is what Finder, Explorer and Office need to write at all). GET and HEAD are served
//! by the web handler, which accepts the same credentials.
//!
//! Rules:
//! - Only from the local networks: requests from the internet are refused, whatever they
//!   carry.
//! - Every request needs Basic auth with an app password; browser sessions don't count.
//! - The same `RelPath` and role × volume permission code as the web: dot-names only for the
//!   admin, chestcraft's internal names never.
//! - DELETE moves to the trash. PUT and MOVE replace a file in place (the old version is
//!   left to snapshots), which needs the right to delete; without it only an empty file
//!   can be replaced. A folder in the way goes to the trash first, as does anything a COPY
//!   replaces.
//! - rclone's `vendor = owncloud` quirks: `X-OC-Mtime` on PUT, PROPPATCH of
//!   `DAV:lastmodified`.

pub mod xml;

use crate::auth::perm::{Origin, Perms, Principal, path_perms, volume_perms};
use crate::fs::ops::{self, EntryKind, OpError};
use crate::fs::path::{PathError, RelPath, encode_segment, validate_new_name};
use crate::fs::transfer::{self, CopySpec, NewFile, OnClash, Placed, Replaced};
use crate::fs::trash;
use crate::fs::volume::{Kind, Meta, Space, VolumeRoot};
use crate::web::{ClientInfo, Shared, audit, now};
use axum::body::{Body, Bytes};
use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
use percent_encoding::percent_decode_str;
use std::io;
use std::sync::Arc;
use xml::{DAV, MS, Name, Propfind};

/// Largest XML request body (PROPFIND, PROPPATCH, LOCK).
const MAX_XML_BODY: usize = 1 << 20;
/// Uploads are written in pieces of at most this size.
const WRITE_BUF: usize = 4 << 20;
const ALLOW: &str =
    "OPTIONS, GET, HEAD, PROPFIND, PROPPATCH, MKCOL, PUT, DELETE, COPY, MOVE, LOCK, UNLOCK";
const LOCK_SECONDS: u32 = 3600;
const XML_TYPE: &str = "application/xml; charset=utf-8";

const SUPPORTED_LOCK: &str = "<D:lockentry><D:lockscope><D:exclusive/></D:lockscope>\
    <D:locktype><D:write/></D:locktype></D:lockentry><D:lockentry><D:lockscope><D:shared/>\
    </D:lockscope><D:locktype><D:write/></D:locktype></D:lockentry>";

/// Live properties returned for `allprop` (RFC 4918 leaves out the quota ones).
const ALLPROP: &[&str] = &[
    "resourcetype",
    "displayname",
    "getcontentlength",
    "getlastmodified",
    "getetag",
    "getcontenttype",
    "supportedlock",
    "lockdiscovery",
];
const QUOTA: &[&str] = &["quota-available-bytes", "quota-used-bytes"];

/// The account behind a WebDAV request.
#[derive(Debug, Clone)]
pub struct DavUser {
    pub key: String,
    pub name: String,
    /// Label of the app password used.
    pub label: String,
}

/// Request extension: set when the request carried a valid app password.
pub type DavAuth = Option<Arc<DavUser>>;

/// Methods answered here rather than by the web handler.
pub fn is_dav_method(m: &http::Method) -> bool {
    matches!(
        m.as_str(),
        "OPTIONS"
            | "PROPFIND"
            | "PROPPATCH"
            | "MKCOL"
            | "PUT"
            | "DELETE"
            | "COPY"
            | "MOVE"
            | "LOCK"
            | "UNLOCK"
    )
}

pub fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(
            header::WWW_AUTHENTICATE,
            "Basic realm=\"chestcraft\", charset=\"UTF-8\"",
        )],
        "WebDAV needs your user name and an app password.\n",
    )
        .into_response()
}

fn plain(code: StatusCode, msg: &'static str) -> Response {
    (
        code,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        msg,
    )
        .into_response()
}

fn status(code: StatusCode) -> Response {
    code.into_response()
}

fn xml_response(code: StatusCode, body: String) -> Response {
    (code, [(header::CONTENT_TYPE, XML_TYPE)], body).into_response()
}

/// HTTP status for an operation error.
fn op_status(e: &OpError) -> StatusCode {
    match e {
        OpError::NotFound => StatusCode::NOT_FOUND,
        OpError::Exists(_) => StatusCode::PRECONDITION_FAILED,
        OpError::Blocked(_) | OpError::Busy | OpError::Incomplete | OpError::Gone => {
            StatusCode::CONFLICT
        }
        OpError::NoSpace => StatusCode::INSUFFICIENT_STORAGE,
        OpError::ReadOnlyInside
        | OpError::VolumeInside
        | OpError::MountPoint
        | OpError::Denied
        | OpError::Invalid(_) => StatusCode::FORBIDDEN,
        OpError::WorkDir { .. } | OpError::Io(_) | OpError::Db(_) => {
            tracing::error!("webdav: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Box<Response>> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| Box::new(status(StatusCode::INTERNAL_SERVER_ERROR)))
}

async fn read_small(body: Body) -> Result<Bytes, Box<Response>> {
    axum::body::to_bytes(body, MAX_XML_BODY)
        .await
        .map_err(|_| Box::new(status(StatusCode::PAYLOAD_TOO_LARGE)))
}

struct Ctx {
    st: Shared,
    client: ClientInfo,
    principal: Principal,
    user: Arc<DavUser>,
}

impl Ctx {
    fn actor(&self) -> String {
        format!("user:{}", self.user.key)
    }

    fn perms(&self, vol: &VolumeRoot, rel: &RelPath) -> Perms {
        path_perms(
            &self.st.cfg,
            &self.principal,
            self.client.origin,
            vol.name(),
            rel,
        )
    }

    fn can_read_volume(&self, vol: &VolumeRoot) -> bool {
        volume_perms(
            &self.st.cfg,
            &self.principal,
            self.client.origin,
            vol.name(),
        )
        .contains(Perms::READ)
    }

    fn audit(&self, action: &str, target: &str, detail: &str) {
        let detail = if detail.is_empty() {
            format!("webdav {}", self.user.label)
        } else {
            format!("{detail}, webdav {}", self.user.label)
        };
        audit(
            &self.st,
            self.actor(),
            self.client.ip,
            action,
            target,
            &detail,
        );
    }
}

enum Target {
    Root,
    Vol(Arc<VolumeRoot>, RelPath),
}

/// Volume and path from a URL path.
fn target(st: &Shared, raw: &str) -> Result<Target, StatusCode> {
    let trimmed = raw.trim_start_matches('/');
    if trimmed.is_empty() {
        return Ok(Target::Root);
    }
    let (seg, rest) = trimmed.split_once('/').unwrap_or((trimmed, ""));
    let name = percent_decode_str(seg)
        .decode_utf8()
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let vol = st
        .volumes
        .get(&name)
        .cloned()
        .ok_or(StatusCode::NOT_FOUND)?;
    match RelPath::from_url(rest) {
        Ok(rel) => Ok(Target::Vol(vol, rel)),
        Err(PathError::BadEncoding) => Err(StatusCode::BAD_REQUEST),
        Err(_) => Err(StatusCode::NOT_FOUND),
    }
}

/// The path of a `Destination` header (an absolute URL or path).
fn destination(headers: &HeaderMap) -> Option<String> {
    let v = headers.get("destination")?.to_str().ok()?.trim();
    if v.starts_with('/') {
        return Some(v.split(['?', '#']).next().unwrap_or(v).to_string());
    }
    Some(v.parse::<http::Uri>().ok()?.path().to_string())
}

fn href(vol: &VolumeRoot, rel: &RelPath, dir: bool) -> String {
    let mut s = format!("/{}/", encode_segment(vol.name()));
    if !rel.is_root() {
        s.push_str(&rel.to_url());
        if dir {
            s.push('/');
        }
    }
    s
}

fn http_date(unix: i64) -> String {
    jiff::Timestamp::from_second(unix)
        .map(|ts| ts.strftime("%a, %d %b %Y %H:%M:%S GMT").to_string())
        .unwrap_or_default()
}

/// Unix seconds (with an optional fraction) → milliseconds.
fn parse_unix(s: &str) -> Option<i64> {
    let secs: f64 = s.trim().parse().ok()?;
    (secs.is_finite() && secs.abs() < 1e11).then(|| (secs * 1000.0).round() as i64)
}

/// An HTTP date (`Wed, 15 Nov 2023 10:00:00 GMT`) → milliseconds.
fn parse_http_date(s: &str) -> Option<i64> {
    let ts = jiff::fmt::rfc2822::DateTimeParser::new()
        .parse_timestamp(s.trim())
        .ok()?;
    Some(ts.as_millisecond())
}

pub async fn handle(st: Shared, req: Request) -> Response {
    let Some(client) = req.extensions().get::<ClientInfo>().copied() else {
        return status(StatusCode::INTERNAL_SERVER_ERROR);
    };
    if client.origin == Origin::Remote {
        return plain(
            StatusCode::FORBIDDEN,
            "WebDAV is only available on the local network.\n",
        );
    }
    let Some(user) = req.extensions().get::<DavAuth>().cloned().flatten() else {
        return unauthorized();
    };
    let principal = req
        .extensions()
        .get::<Principal>()
        .cloned()
        .unwrap_or(Principal::Anonymous);
    let (parts, body) = req.into_parts();
    let ctx = Ctx {
        st: st.clone(),
        client,
        principal,
        user,
    };
    let method = parts.method.as_str();
    if method == "OPTIONS" {
        return options();
    }
    let tgt = match target(&st, parts.uri.path()) {
        Ok(t) => t,
        Err(code) => return status(code),
    };
    if let Target::Vol(vol, _) = &tgt
        && !ctx.can_read_volume(vol)
    {
        return status(StatusCode::NOT_FOUND);
    }
    let h = &parts.headers;
    match method {
        "PROPFIND" => propfind(&ctx, tgt, h, body).await,
        "PROPPATCH" => proppatch(&ctx, tgt, body).await,
        "MKCOL" => mkcol(&ctx, tgt, body).await,
        "PUT" => put(&ctx, tgt, h, body).await,
        "DELETE" => delete(&ctx, tgt).await,
        "COPY" => copy_move(&ctx, tgt, h, false).await,
        "MOVE" => copy_move(&ctx, tgt, h, true).await,
        "LOCK" => lock(&ctx, tgt, parts.uri.path(), h, body).await,
        "UNLOCK" => status(StatusCode::NO_CONTENT),
        _ => (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, ALLOW)]).into_response(),
    }
}

fn options() -> Response {
    (
        StatusCode::OK,
        [
            (header::ALLOW, ALLOW),
            (header::HeaderName::from_static("dav"), "1, 2"),
            (header::HeaderName::from_static("ms-author-via"), "DAV"),
        ],
    )
        .into_response()
}

// ---------------------------------------------------------------------------------------
// PROPFIND

struct Res {
    href: String,
    name: String,
    dir: bool,
    /// None for the virtual root.
    meta: Option<Meta>,
    quota: Option<Space>,
}

fn prop_value(r: &Res, n: &Name) -> Option<String> {
    if n.ns != DAV {
        return None;
    }
    match n.local.as_str() {
        "resourcetype" => Some(if r.dir { "<D:collection/>" } else { "" }.to_string()),
        "displayname" => Some(xml::esc(&r.name)),
        "getcontentlength" if !r.dir => r.meta.as_ref().map(|m| m.size.to_string()),
        "getlastmodified" => r.meta.as_ref().map(|m| http_date(m.mtime)),
        "getetag" => r.meta.as_ref().map(|m| xml::esc(&m.etag())),
        "getcontenttype" if !r.dir => Some(
            mime_guess::from_path(&r.name)
                .first_or_octet_stream()
                .essence_str()
                .to_string(),
        ),
        "supportedlock" => Some(SUPPORTED_LOCK.to_string()),
        "lockdiscovery" => Some(String::new()),
        "quota-available-bytes" => r.quota.map(|q| q.avail.to_string()),
        "quota-used-bytes" => r.quota.map(|q| q.total.saturating_sub(q.avail).to_string()),
        _ => None,
    }
}

fn wants_quota(req: &Propfind) -> bool {
    match req {
        Propfind::AllProp => false,
        Propfind::PropName => true,
        Propfind::Props(names) => names
            .iter()
            .any(|n| n.ns == DAV && QUOTA.contains(&n.local.as_str())),
    }
}

fn write_response(out: &mut String, r: &Res, req: &Propfind) {
    out.push_str("<D:response><D:href>");
    out.push_str(&xml::esc(&r.href));
    out.push_str("</D:href>");
    let (mut found, mut missing) = (String::new(), String::new());
    match req {
        Propfind::AllProp => {
            for local in ALLPROP {
                let n = Name::dav(local);
                if let Some(v) = prop_value(r, &n) {
                    xml::element(&mut found, &n, Some(&v));
                }
            }
        }
        Propfind::PropName => {
            for local in ALLPROP.iter().chain(QUOTA) {
                let n = Name::dav(local);
                if prop_value(r, &n).is_some() {
                    xml::element(&mut found, &n, None);
                }
            }
        }
        Propfind::Props(names) => {
            for n in names {
                match prop_value(r, n) {
                    Some(v) => xml::element(&mut found, n, Some(&v)),
                    None => xml::element(&mut missing, n, None),
                }
            }
        }
    }
    // rclone reads the status of the first propstat, so the found ones come first.
    if !found.is_empty() || missing.is_empty() {
        xml::propstat(out, StatusCode::OK, &found);
    }
    if !missing.is_empty() {
        xml::propstat(out, StatusCode::NOT_FOUND, &missing);
    }
    out.push_str("</D:response>");
}

async fn propfind(ctx: &Ctx, tgt: Target, headers: &HeaderMap, body: Body) -> Response {
    let depth = match headers
        .get("depth")
        .map(|v| v.to_str().unwrap_or("?").trim())
    {
        // RFC 4918 says infinity, but clients that leave it out expect a listing.
        None | Some("1") => 1,
        Some("0") => 0,
        Some(d) if d.eq_ignore_ascii_case("infinity") => {
            return xml_response(
                StatusCode::FORBIDDEN,
                format!(
                    "{}<D:error xmlns:D=\"DAV:\"><D:propfind-finite-depth/></D:error>",
                    xml::HEADER
                ),
            );
        }
        Some(_) => return status(StatusCode::BAD_REQUEST),
    };
    let body = match read_small(body).await {
        Ok(b) => b,
        Err(r) => return *r,
    };
    let Ok(req) = xml::propfind(&body) else {
        return status(StatusCode::BAD_REQUEST);
    };

    let found = match tgt {
        Target::Root => {
            let vols: Vec<Arc<VolumeRoot>> = ctx
                .st
                .volumes
                .iter()
                .filter(|v| ctx.can_read_volume(v))
                .cloned()
                .collect();
            let quota = wants_quota(&req);
            blocking(move || -> io::Result<Vec<Res>> {
                // rclone asks the root for `about` and `df` on a mount: report the volume
                // with the most free space (volumes on one pool share it; adding up would
                // count it twice).
                let quota = if quota {
                    vols.iter()
                        .filter_map(|v| v.space().ok())
                        .max_by_key(|q| (q.avail, q.total))
                } else {
                    None
                };
                let mut out = vec![Res {
                    href: "/".into(),
                    name: String::new(),
                    dir: true,
                    meta: None,
                    quota,
                }];
                for v in vols.into_iter().filter(|_| depth == 1) {
                    // An unreachable volume (sick pool) is left out rather than failing all.
                    let Ok(meta) = v.stat(&RelPath::root()) else {
                        continue;
                    };
                    out.push(Res {
                        href: href(&v, &RelPath::root(), true),
                        name: v.name().to_string(),
                        dir: true,
                        meta: Some(meta),
                        quota: v.space().ok(),
                    });
                }
                Ok(out)
            })
            .await
        }
        Target::Vol(vol, rel) => {
            if !ctx.perms(&vol, &rel).contains(Perms::READ) {
                return status(StatusCode::NOT_FOUND);
            }
            let admin = ctx.principal.is_admin();
            blocking(move || -> io::Result<Vec<Res>> {
                let meta = vol.stat(&rel)?;
                if meta.kind == Kind::Other {
                    return Err(io::ErrorKind::NotFound.into());
                }
                let dir = meta.kind == Kind::Dir;
                let quota = if dir { vol.space().ok() } else { None };
                let mut out = vec![Res {
                    href: href(&vol, &rel, dir),
                    name: rel.name().unwrap_or(vol.name()).to_string(),
                    dir,
                    meta: Some(meta),
                    quota,
                }];
                if dir && depth == 1 {
                    for e in vol.list(&rel, admin)? {
                        if e.meta.kind == Kind::Other {
                            continue;
                        }
                        let Ok(child) = rel.join(&e.name) else {
                            continue;
                        };
                        let dir = e.meta.kind == Kind::Dir;
                        out.push(Res {
                            href: href(&vol, &child, dir),
                            name: e.name,
                            dir,
                            meta: Some(e.meta),
                            quota: if dir { quota } else { None },
                        });
                    }
                }
                Ok(out)
            })
            .await
        }
    };
    let resources = match found {
        Ok(Ok(r)) => r,
        Ok(Err(e)) if e.kind() == io::ErrorKind::NotFound => {
            return status(StatusCode::NOT_FOUND);
        }
        Ok(Err(e)) => {
            tracing::warn!("webdav listing failed: {e}");
            return status(StatusCode::INTERNAL_SERVER_ERROR);
        }
        Err(r) => return *r,
    };
    let mut out = String::with_capacity(512 * resources.len() + 128);
    out.push_str(xml::HEADER);
    out.push_str("<D:multistatus xmlns:D=\"DAV:\">");
    for r in &resources {
        write_response(&mut out, r, &req);
    }
    out.push_str("</D:multistatus>");
    xml_response(StatusCode::MULTI_STATUS, out)
}

// ---------------------------------------------------------------------------------------
// PROPPATCH

async fn proppatch(ctx: &Ctx, tgt: Target, body: Body) -> Response {
    let Target::Vol(vol, rel) = tgt else {
        return status(StatusCode::FORBIDDEN);
    };
    let perms = ctx.perms(&vol, &rel);
    if !perms.contains(Perms::READ) {
        return status(StatusCode::NOT_FOUND);
    }
    let body = match read_small(body).await {
        Ok(b) => b,
        Err(r) => return *r,
    };
    let Ok(ops) = xml::proppatch(&body) else {
        return status(StatusCode::BAD_REQUEST);
    };
    let meta = {
        let (vol, rel) = (vol.clone(), rel.clone());
        match blocking(move || vol.stat(&rel)).await {
            Ok(Ok(m)) => m,
            Ok(Err(_)) => return status(StatusCode::NOT_FOUND),
            Err(r) => return *r,
        }
    };

    let mut mtime = None;
    let mut results: Vec<(Name, StatusCode)> = Vec::new();
    for op in &ops {
        let n = &op.name;
        let code = if !op.set || !perms.contains(Perms::WRITE) {
            StatusCode::FORBIDDEN
        } else {
            let ms = match (n.ns.as_str(), n.local.as_str()) {
                (DAV, "lastmodified") => Some(op.value.as_deref().and_then(parse_unix)),
                (DAV, "getlastmodified") | (MS, "Win32LastModifiedTime") => {
                    Some(op.value.as_deref().and_then(parse_http_date))
                }
                _ => None,
            };
            match ms {
                Some(Some(ms)) => {
                    mtime = Some(ms);
                    StatusCode::OK
                }
                Some(None) => StatusCode::CONFLICT,
                // Windows sets these with every file; claiming success keeps it working.
                None if n.ns == MS
                    && matches!(
                        n.local.as_str(),
                        "Win32CreationTime" | "Win32LastAccessTime" | "Win32FileAttributes"
                    ) =>
                {
                    StatusCode::OK
                }
                None => StatusCode::FORBIDDEN,
            }
        };
        results.push((n.clone(), code));
    }
    if results.iter().any(|(_, c)| *c != StatusCode::OK) {
        // All or nothing: the ones that would have worked fail too.
        for (_, c) in &mut results {
            if *c == StatusCode::OK {
                *c = StatusCode::FAILED_DEPENDENCY;
            }
        }
    } else if let Some(ms) = mtime {
        let (v, r) = (vol.clone(), rel.clone());
        let applied = match blocking(move || transfer::set_mtime(&v, &r, ms)).await {
            Ok(res) => res,
            Err(r) => return *r,
        };
        if let Err(e) = applied {
            let code = match &e {
                OpError::Io(io) if ops::errno(io) == Some(rustix::io::Errno::PERM) => {
                    StatusCode::FORBIDDEN
                }
                other => op_status(other),
            };
            for (_, c) in &mut results {
                *c = code;
            }
        } else {
            ctx.audit(
                "set_mtime",
                &format!("{}/{}", vol.name(), rel.as_str()),
                &http_date(ms.div_euclid(1000)),
            );
        }
    }

    let mut out = String::new();
    out.push_str(xml::HEADER);
    out.push_str("<D:multistatus xmlns:D=\"DAV:\"><D:response><D:href>");
    out.push_str(&xml::esc(&href(&vol, &rel, meta.kind == Kind::Dir)));
    out.push_str("</D:href>");
    let mut codes: Vec<StatusCode> = results.iter().map(|(_, c)| *c).collect();
    codes.sort_by_key(|c| c.as_u16());
    codes.dedup();
    for code in codes {
        let mut props = String::new();
        for (n, _) in results.iter().filter(|(_, c)| *c == code) {
            xml::element(&mut props, n, None);
        }
        xml::propstat(&mut out, code, &props);
    }
    out.push_str("</D:response></D:multistatus>");
    xml_response(StatusCode::MULTI_STATUS, out)
}

// ---------------------------------------------------------------------------------------
// MKCOL, PUT, DELETE

async fn mkcol(ctx: &Ctx, tgt: Target, body: Body) -> Response {
    let Target::Vol(vol, rel) = tgt else {
        return status(StatusCode::METHOD_NOT_ALLOWED);
    };
    let (Some(parent), Some(name)) = (rel.parent(), rel.name().map(str::to_string)) else {
        return status(StatusCode::METHOD_NOT_ALLOWED);
    };
    match read_small(body).await {
        Ok(b) if b.is_empty() => {}
        Ok(_) => return status(StatusCode::UNSUPPORTED_MEDIA_TYPE),
        Err(r) => return *r,
    }
    if validate_new_name(&name).is_err() || !ctx.perms(&vol, &rel).contains(Perms::WRITE) {
        return status(StatusCode::FORBIDDEN);
    }
    let made = {
        let vol = vol.clone();
        blocking(move || -> Result<(), StatusCode> {
            let pfd = vol.open_dir(&parent).map_err(|e| match e.kind() {
                io::ErrorKind::NotFound => StatusCode::CONFLICT,
                _ => op_status(&e.into()),
            })?;
            ops::mkdir_in(&pfd, &name).map_err(|e| match e {
                OpError::Exists(_) => StatusCode::METHOD_NOT_ALLOWED,
                other => op_status(&other),
            })
        })
        .await
    };
    match made {
        Ok(Ok(())) => {
            ctx.audit("mkdir", &format!("{}/{}", vol.name(), rel.as_str()), "");
            status(StatusCode::CREATED)
        }
        Ok(Err(code)) => status(code),
        Err(r) => *r,
    }
}

/// Give up on a file being written: dropping it removes the temporary file, which may
/// take a while for a big one, so not on an async worker.
fn discard(file: NewFile, code: StatusCode) -> Response {
    tokio::task::spawn_blocking(move || drop(file));
    status(code)
}

fn etag_listed(list: &str, etag: &str) -> bool {
    list.split(',').any(|t| t.trim() == "*" || t.trim() == etag)
}

async fn put(ctx: &Ctx, tgt: Target, headers: &HeaderMap, body: Body) -> Response {
    let Target::Vol(vol, rel) = tgt else {
        return status(StatusCode::METHOD_NOT_ALLOWED);
    };
    let (Some(parent), Some(name)) = (rel.parent(), rel.name().map(str::to_string)) else {
        return status(StatusCode::METHOD_NOT_ALLOWED);
    };
    if headers.contains_key(header::CONTENT_RANGE) {
        return plain(
            StatusCode::BAD_REQUEST,
            "Partial uploads (Content-Range) aren't supported.\n",
        );
    }
    let perms = ctx.perms(&vol, &rel);
    if validate_new_name(&name).is_err() || !perms.contains(Perms::WRITE) {
        return status(StatusCode::FORBIDDEN);
    }
    let text = |h: header::HeaderName| headers.get(h).and_then(|v| v.to_str().ok()).map(str::trim);
    let create_only = text(header::IF_NONE_MATCH) == Some("*");
    let if_match = text(header::IF_MATCH).map(str::to_string);
    let on_clash = if create_only {
        OnClash::Fail
    } else if perms.contains(Perms::DELETE) {
        OnClash::Overwrite
    } else {
        OnClash::ReplaceEmpty
    };
    let size = text(header::CONTENT_LENGTH).and_then(|v| v.parse::<u64>().ok());
    let oc_mtime = headers
        .get("x-oc-mtime")
        .and_then(|v| v.to_str().ok())
        .and_then(parse_unix);

    let started = {
        let (db, vol, name) = (ctx.st.db.clone(), vol.clone(), name.clone());
        blocking(move || -> Result<NewFile, StatusCode> {
            let pfd = vol.open_dir(&parent).map_err(|e| match e.kind() {
                io::ErrorKind::NotFound => StatusCode::CONFLICT,
                _ => op_status(&e.into()),
            })?;
            match ops::lstat(&pfd, &name) {
                Ok((EntryKind::Dir, _)) => return Err(StatusCode::METHOD_NOT_ALLOWED),
                Ok((_, st)) => {
                    if create_only {
                        return Err(StatusCode::PRECONDITION_FAILED);
                    }
                    if let Some(m) = &if_match
                        && !etag_listed(m, &Meta::from_stat(&st).etag())
                    {
                        return Err(StatusCode::PRECONDITION_FAILED);
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    if if_match.is_some() {
                        return Err(StatusCode::PRECONDITION_FAILED);
                    }
                }
                Err(e) => return Err(op_status(&e.into())),
            }
            NewFile::create(&db, &vol, &parent, &name, size, now()).map_err(|e| op_status(&e))
        })
        .await
    };
    let mut file = match started {
        Ok(Ok(f)) => f,
        Ok(Err(code)) => return status(code),
        Err(r) => return *r,
    };

    let mut stream = body.into_data_stream();
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let next = stream.next().await;
        let flush = match &next {
            Some(Ok(data)) => {
                buf.extend_from_slice(data);
                buf.len() >= WRITE_BUF
            }
            // The client went away.
            Some(Err(_)) => return discard(file, StatusCode::BAD_REQUEST),
            None => !buf.is_empty(),
        };
        if flush {
            let data = std::mem::take(&mut buf);
            let (f, res, mut data) = match blocking(move || {
                let res = file.write_all(&data);
                (file, res, data)
            })
            .await
            {
                Ok(x) => x,
                Err(r) => return *r,
            };
            file = f;
            if let Err(e) = res {
                return discard(file, op_status(&e));
            }
            data.clear();
            buf = data;
        }
        if next.is_none() {
            break;
        }
    }
    let written = file.written();
    if size.is_some_and(|n| n != written) {
        return discard(file, StatusCode::BAD_REQUEST);
    }
    let actor = ctx.actor();
    let placed = match blocking(move || file.commit(oc_mtime, on_clash, &actor, now())).await {
        Ok(p) => p,
        Err(r) => return *r,
    };
    let code = match placed {
        Ok(Placed::Done { replaced, .. }) => {
            let detail = match replaced {
                Replaced::Nothing => format!("{written} bytes"),
                Replaced::InPlace => format!("{written} bytes, replaced"),
                Replaced::Trashed(_) => format!("{written} bytes, old one in the trash"),
            };
            ctx.audit(
                "upload",
                &format!("{}/{}", vol.name(), rel.as_str()),
                &detail,
            );
            if replaced == Replaced::Nothing {
                StatusCode::CREATED
            } else {
                StatusCode::NO_CONTENT
            }
        }
        Ok(Placed::Skipped) => StatusCode::PRECONDITION_FAILED,
        // Only an empty file may be replaced without the right to delete.
        Err(OpError::Exists(_)) if on_clash == OnClash::ReplaceEmpty => StatusCode::FORBIDDEN,
        Err(OpError::NotFound) => StatusCode::CONFLICT,
        Err(e) => op_status(&e),
    };
    let mut res = status(code);
    if oc_mtime.is_some() && code.is_success() {
        res.headers_mut().insert(
            header::HeaderName::from_static("x-oc-mtime"),
            HeaderValue::from_static("accepted"),
        );
    }
    res
}

async fn delete(ctx: &Ctx, tgt: Target) -> Response {
    let Target::Vol(vol, rel) = tgt else {
        return status(StatusCode::FORBIDDEN);
    };
    if rel.is_root() {
        return status(StatusCode::FORBIDDEN);
    }
    let perms = ctx.perms(&vol, &rel);
    if !perms.contains(Perms::READ) {
        return status(StatusCode::NOT_FOUND);
    }
    if !perms.contains(Perms::DELETE) {
        return status(StatusCode::FORBIDDEN);
    }
    let done = {
        let (st, vol, rel, actor) = (ctx.st.clone(), vol.clone(), rel.clone(), ctx.actor());
        blocking(move || trash::delete(&st.db, &vol, &rel, &actor, trash::REASON_DELETE, now()))
            .await
    };
    match done {
        Ok(Ok(item)) => {
            ctx.audit(
                "trash",
                &format!("{}/{}", item.volume, item.path),
                &format!("{} bytes, {} files", item.size, item.files),
            );
            status(StatusCode::NO_CONTENT)
        }
        Ok(Err(e)) => status(op_status(&e)),
        Err(r) => *r,
    }
}

// ---------------------------------------------------------------------------------------
// COPY, MOVE

async fn copy_move(ctx: &Ctx, tgt: Target, headers: &HeaderMap, is_move: bool) -> Response {
    let Target::Vol(svol, src) = tgt else {
        return status(StatusCode::FORBIDDEN);
    };
    if is_move && src.is_root() {
        return status(StatusCode::FORBIDDEN);
    }
    let sperms = ctx.perms(&svol, &src);
    if !sperms.contains(Perms::READ) {
        return status(StatusCode::NOT_FOUND);
    }
    if is_move && !sperms.contains(Perms::MOVE) {
        return status(StatusCode::FORBIDDEN);
    }
    let Some(dpath) = destination(headers) else {
        return status(StatusCode::BAD_REQUEST);
    };
    let (dvol, dst) = match target(&ctx.st, &dpath) {
        Ok(Target::Vol(v, r)) if ctx.can_read_volume(&v) => (v, r),
        Ok(_) | Err(StatusCode::NOT_FOUND) => return status(StatusCode::FORBIDDEN),
        Err(code) => return status(code),
    };
    let (Some(dparent), Some(dname)) = (dst.parent(), dst.name().map(str::to_string)) else {
        return status(StatusCode::FORBIDDEN);
    };
    if validate_new_name(&dname).is_err() || (svol.name() == dvol.name() && src == dst) {
        return status(StatusCode::FORBIDDEN);
    }
    let text = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
    };
    let overwrite = match text("overwrite") {
        None | Some("T" | "t") => true,
        Some("F" | "f") => false,
        Some(_) => return status(StatusCode::BAD_REQUEST),
    };
    let shallow = match text("depth") {
        None => false,
        Some(d) if d.eq_ignore_ascii_case("infinity") => false,
        Some("0") if !is_move => true,
        Some(_) => return status(StatusCode::BAD_REQUEST),
    };
    let dperms = ctx.perms(&dvol, &dst);
    let needed = if is_move && svol.name() == dvol.name() {
        Perms::MOVE
    } else {
        Perms::WRITE
    };
    if !dperms.contains(needed) {
        return status(StatusCode::FORBIDDEN);
    }
    let on_clash = match (overwrite, dperms.contains(Perms::DELETE), is_move) {
        (false, _, _) => OnClash::Fail,
        (true, false, _) => OnClash::ReplaceEmpty,
        (true, true, true) => OnClash::Overwrite,
        (true, true, false) => OnClash::Trash,
    };
    let spec = CopySpec {
        include_hidden: ctx.principal.is_admin(),
        shallow,
    };

    let done = {
        let (db, svol, src, dvol, actor) = (
            ctx.st.db.clone(),
            svol.clone(),
            src.clone(),
            dvol.clone(),
            ctx.actor(),
        );
        blocking(move || -> Result<Placed, StatusCode> {
            match svol.stat(&src) {
                Ok(m) if m.kind != Kind::Other => {}
                Ok(_) => return Err(StatusCode::FORBIDDEN),
                Err(_) => return Err(StatusCode::NOT_FOUND),
            }
            match dvol.stat(&dparent) {
                Ok(m) if m.kind == Kind::Dir => {}
                _ => return Err(StatusCode::CONFLICT),
            }
            let result = if is_move {
                transfer::move_entry(
                    &db,
                    &svol,
                    &src,
                    &dvol,
                    &dparent,
                    &dname,
                    on_clash,
                    &actor,
                    now(),
                )
            } else {
                transfer::copy_entry(
                    &db,
                    &svol,
                    &src,
                    &dvol,
                    &dparent,
                    &dname,
                    spec,
                    on_clash,
                    &actor,
                    now(),
                )
            };
            result.map_err(|e| match e {
                OpError::Exists(_) if on_clash == OnClash::ReplaceEmpty => StatusCode::FORBIDDEN,
                other => op_status(&other),
            })
        })
        .await
    };
    match done {
        Ok(Ok(Placed::Done { replaced, .. })) => {
            let mut detail = format!("to {}/{}", dvol.name(), dst.as_str());
            match replaced {
                Replaced::Nothing => {}
                Replaced::InPlace => detail.push_str(", replaced"),
                Replaced::Trashed(_) => detail.push_str(", old one in the trash"),
            }
            ctx.audit(
                if is_move { "move" } else { "copy" },
                &format!("{}/{}", svol.name(), src.as_str()),
                &detail,
            );
            status(if replaced == Replaced::Nothing {
                StatusCode::CREATED
            } else {
                StatusCode::NO_CONTENT
            })
        }
        Ok(Ok(Placed::Skipped)) => status(StatusCode::PRECONDITION_FAILED),
        Ok(Err(code)) => status(code),
        Err(r) => *r,
    }
}

// ---------------------------------------------------------------------------------------
// LOCK

fn new_lock_token() -> String {
    use crate::auth::random::{bytes, hex};
    format!(
        "opaquelocktoken:{}-{}-{}-{}-{}",
        hex(&bytes::<4>()),
        hex(&bytes::<2>()),
        hex(&bytes::<2>()),
        hex(&bytes::<2>()),
        hex(&bytes::<6>())
    )
}

/// The lock token in an `If` header, for refreshes.
fn token_in_if(headers: &HeaderMap) -> Option<String> {
    let v = headers.get("if")?.to_str().ok()?;
    let start = v.find("<opaquelocktoken:")? + 1;
    let len = v[start..].find('>')?;
    let token = &v[start..start + len];
    token
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b':' || b == b'-')
        .then(|| token.to_string())
}

async fn lock(ctx: &Ctx, tgt: Target, path: &str, headers: &HeaderMap, body: Body) -> Response {
    let Target::Vol(vol, rel) = tgt else {
        return status(StatusCode::FORBIDDEN);
    };
    if !ctx.perms(&vol, &rel).contains(Perms::READ) {
        return status(StatusCode::NOT_FOUND);
    }
    let body = match read_small(body).await {
        Ok(b) => b,
        Err(r) => return *r,
    };
    let info = match xml::lockinfo(&body) {
        Ok(i) => i,
        Err(_) => return status(StatusCode::BAD_REQUEST),
    };
    let token = match info {
        None => token_in_if(headers).unwrap_or_else(new_lock_token),
        Some(_) => new_lock_token(),
    };
    let scope = if info.is_some_and(|i| i.shared) {
        "shared"
    } else {
        "exclusive"
    };
    let depth = match headers.get("depth").and_then(|v| v.to_str().ok()) {
        Some("0") => "0",
        _ => "infinity",
    };
    let out = format!(
        "{}<D:prop xmlns:D=\"DAV:\"><D:lockdiscovery><D:activelock>\
         <D:locktype><D:write/></D:locktype><D:lockscope><D:{scope}/></D:lockscope>\
         <D:depth>{depth}</D:depth><D:timeout>Second-{LOCK_SECONDS}</D:timeout>\
         <D:locktoken><D:href>{token}</D:href></D:locktoken>\
         <D:lockroot><D:href>{}</D:href></D:lockroot>\
         </D:activelock></D:lockdiscovery></D:prop>",
        xml::HEADER,
        xml::esc(path)
    );
    let mut res = xml_response(StatusCode::OK, out);
    if let Ok(v) = HeaderValue::from_str(&format!("<{token}>")) {
        res.headers_mut()
            .insert(header::HeaderName::from_static("lock-token"), v);
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_and_tokens() {
        assert_eq!(parse_unix("1700000000"), Some(1_700_000_000_000));
        assert_eq!(parse_unix(" 1.5 "), Some(1500));
        assert_eq!(parse_unix("nan"), None);
        assert_eq!(parse_unix("1e30"), None);
        assert_eq!(
            parse_http_date("Wed, 15 Nov 2023 10:00:00 GMT"),
            Some(1_700_042_400_000)
        );
        assert_eq!(parse_http_date("yesterday"), None);
        assert_eq!(http_date(1_700_042_400), "Wed, 15 Nov 2023 10:00:00 GMT");
        let t = new_lock_token();
        let mut h = HeaderMap::new();
        h.insert("if", format!("(<{t}>)").parse().unwrap());
        assert_eq!(token_in_if(&h), Some(t));
        h.insert("if", "(<opaquelocktoken:\"><script>)".parse().unwrap());
        assert_eq!(token_in_if(&h), None);
        assert!(etag_listed("\"a\", \"b\"", "\"b\"") && etag_listed("*", "\"x\""));
        assert!(!etag_listed("\"a\"", "\"b\""));
    }

    #[test]
    fn destinations() {
        let mut h = HeaderMap::new();
        h.insert(
            "destination",
            "http://192.0.2.5:3923/Medien/neu%20er/x.txt"
                .parse()
                .unwrap(),
        );
        assert_eq!(destination(&h).as_deref(), Some("/Medien/neu%20er/x.txt"));
        h.insert("destination", "/Medien/a?b".parse().unwrap());
        assert_eq!(destination(&h).as_deref(), Some("/Medien/a"));
        assert_eq!(destination(&HeaderMap::new()), None);
    }
}
