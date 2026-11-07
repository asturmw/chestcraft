//! Static files embedded at build time (see build.rs), served under /.cc/assets/.

use axum::extract::Path;
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use std::collections::HashMap;
use std::sync::LazyLock;

include!(concat!(env!("OUT_DIR"), "/assets.rs"));

struct Asset {
    bytes: &'static [u8],
    hash: String,
}

static INDEX: LazyLock<HashMap<&'static str, Asset>> = LazyLock::new(|| {
    ASSETS
        .iter()
        .map(|(name, bytes)| {
            (
                *name,
                Asset {
                    bytes,
                    hash: format!("{:016x}", fnv1a(bytes)),
                },
            )
        })
        .collect()
});

fn fnv1a(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// URL of an asset with a content version, so browsers may cache it for a year.
pub fn url(path: &str) -> String {
    match INDEX.get(path) {
        Some(a) => format!("/.cc/assets/{path}?v={}", &a.hash[..12]),
        None => format!("/.cc/assets/{path}"),
    }
}

/// Is `path` embedded?
pub fn has(path: &str) -> bool {
    INDEX.contains_key(path)
}

/// Stylesheet URL of a skin, if that skin is embedded.
pub fn skin_css(skin: &str) -> Option<String> {
    let path = format!("skins/{skin}.css");
    INDEX.contains_key(path.as_str()).then(|| url(&path))
}

/// Skins found in the embedded assets (`skins/<name>.css`), sorted.
pub fn skins() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = INDEX
        .keys()
        .filter_map(|k| k.strip_prefix("skins/")?.strip_suffix(".css"))
        .collect();
    v.sort_unstable();
    v
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("jpg") => "image/jpeg",
        Some("woff2") => "font/woff2",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("webmanifest") => "application/manifest+json",
        Some("json") => "application/json",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

pub async fn serve(Path(path): Path<String>, uri: Uri, headers: HeaderMap) -> Response {
    let Some(asset) = INDEX.get(path.as_str()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let etag = format!("\"{}\"", asset.hash);
    let version = format!("v={}", &asset.hash[..12]);
    let versioned = uri
        .query()
        .is_some_and(|q| q.split('&').any(|kv| kv == version));
    let cache = if versioned {
        "public, max-age=31536000, immutable"
    } else {
        "public, max-age=3600"
    };
    let not_modified = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim() == etag || t.trim() == "*"));
    if not_modified {
        return (
            StatusCode::NOT_MODIFIED,
            [(header::ETAG, etag), (header::CACHE_CONTROL, cache.into())],
        )
            .into_response();
    }
    (
        [
            (header::CONTENT_TYPE, content_type(&path).to_string()),
            (header::ETAG, etag),
            (header::CACHE_CONTROL, cache.to_string()),
        ],
        asset.bytes,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The skin default backgrounds from the design branch are embedded, a light twin for
    /// every dark one; they take effect once the skin stylesheets are embedded too.
    #[test]
    fn skin_pictures_are_embedded_in_both_themes() {
        let skins: Vec<String> = ["anime", "classic", "gaming", "modern", "sleek"]
            .map(String::from)
            .into();
        let p = crate::appearance::skin_pictures(&skins, has);
        assert_eq!(
            p.keys().map(String::as_str).collect::<Vec<_>>(),
            ["anime", "gaming", "sleek"]
        );
        for (skin, pic) in &p {
            assert_eq!(pic.dark, format!("backgrounds/default-{skin}-dark.svg"));
            assert_eq!(
                pic.light.as_deref(),
                Some(format!("backgrounds/default-{skin}-light.svg").as_str())
            );
            assert_eq!(content_type(&pic.dark), "image/svg+xml");
        }
    }
}
