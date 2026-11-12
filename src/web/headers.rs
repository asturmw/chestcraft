//! Response headers every response gets.

use super::client::ClientInfo;
use axum::extract::Request;
use axum::http::header::{
    CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, REFERRER_POLICY,
    STRICT_TRANSPORT_SECURITY, X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
};
use axum::http::{HeaderName, HeaderValue};
use axum::middleware::Next;
use axum::response::Response;

/// Policy for chestcraft's own pages: only same-origin resources, no inline script or style.
pub const PAGE_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; \
    img-src 'self' data: blob:; media-src 'self' blob:; font-src 'self'; connect-src 'self'; \
    frame-src 'self'; object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

/// Policy for user files of active types (HTML, SVG, XML): sandboxed, no scripts.
pub const SANDBOX_CSP: &str =
    "sandbox; default-src 'none'; img-src 'self' data:; style-src 'unsafe-inline'";

/// `Strict-Transport-Security`: one year, apply to subdomains too.
pub const HSTS: &str = "max-age=31536000; includeSubDomains";

/// `Permissions-Policy`: deny powerful features we never use.
pub const PERMISSIONS_POLICY: &str = "camera=(), microphone=(), geolocation=(), payment=(), usb=(), magnetometer=(), \
     gyroscope=(), accelerometer=()";

pub async fn security(req: Request, next: Next) -> Response {
    let https = req
        .extensions()
        .get::<ClientInfo>()
        .is_some_and(|c| c.https);
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.entry(X_CONTENT_TYPE_OPTIONS)
        .or_insert(HeaderValue::from_static("nosniff"));
    // Share links and folder names must not leak to other sites. Not `no-referrer`: with
    // it, browsers send `Origin: null` on a form POST (Fetch spec, "append a request Origin
    // header"), and the same-origin check refuses every form.
    h.entry(REFERRER_POLICY)
        .or_insert(HeaderValue::from_static("same-origin"));
    h.entry(HeaderName::from_static("x-robots-tag"))
        .or_insert(HeaderValue::from_static("noindex, nofollow"));
    // `private` keeps Cloudflare and other shared caches from storing anything: a cached
    // private file could otherwise be served to the next visitor without a login.
    h.entry(CACHE_CONTROL)
        .or_insert(HeaderValue::from_static("private, no-cache"));
    h.entry(HeaderName::from_static("permissions-policy"))
        .or_insert(HeaderValue::from_static(PERMISSIONS_POLICY));
    h.entry(HeaderName::from_static("cross-origin-opener-policy"))
        .or_insert(HeaderValue::from_static("same-origin"));
    h.entry(HeaderName::from_static("cross-origin-resource-policy"))
        .or_insert(HeaderValue::from_static("same-origin"));
    // HSTS only on HTTPS requests (confirmed via a trusted proxy's `X-Forwarded-Proto`):
    // over plain HTTP the header has no effect and only misleads clients.
    if https {
        h.entry(STRICT_TRANSPORT_SECURITY)
            .or_insert(HeaderValue::from_static(HSTS));
    }
    let html = h
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("text/html"));
    if html && !h.contains_key(CONTENT_SECURITY_POLICY) {
        h.insert(CONTENT_SECURITY_POLICY, HeaderValue::from_static(PAGE_CSP));
        h.insert(X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    }
    res
}
