//! HTTP tests against the full router with temp-dir volumes.

use super::*;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use std::io::Cursor;
use std::net::SocketAddr;
use tower::ServiceExt;

struct Env {
    _tmp: tempfile::TempDir,
    state: Shared,
}

struct Res {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Res {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    fn header(&self, name: &str) -> &str {
        self.headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
    }
}

fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let r = tmp.path();
    std::fs::create_dir_all(r.join("medien/Urlaub Gardasee")).unwrap();
    std::fs::write(r.join("medien/Urlaub Gardasee/IMG_2041.jpg"), b"jpegdata").unwrap();
    std::fs::write(r.join("medien/a.txt"), b"0123456789").unwrap();
    std::fs::write(r.join("medien/page.html"), b"<script>alert(1)</script>").unwrap();
    std::fs::create_dir(r.join("medien/.git")).unwrap();
    std::fs::create_dir(r.join("medien/.chestcraft-trash")).unwrap();
    std::fs::write(r.join("outside.txt"), b"secret").unwrap();
    std::os::unix::fs::symlink(r.join("outside.txt"), r.join("medien/escape")).unwrap();
    std::fs::create_dir_all(r.join("share")).unwrap();
    std::fs::write(r.join("share/hello.txt"), b"hello").unwrap();
    std::fs::create_dir_all(r.join("backups")).unwrap();
    std::fs::write(r.join("backups/dump.bin"), b"x").unwrap();
    let cfg = Config::from_toml_str(&format!(
        r#"
        [server]
        public_url = "https://files.example.org"
        trusted_proxies = ["192.0.2.10", "192.0.2.11"]
        remote_proxies = ["192.0.2.10"]
        [paths]
        state_dir = "{0}/state"
        cache_dir = "{0}/cache"
        backup_dir = "{0}/backup"
        [volumes.Medien]
        path = "{0}/medien"
        note.en = "Media files"
        [volumes.Share]
        path = "{0}/share"
        public = true
        [volumes.Backups]
        path = "{0}/backups"
        remote = false
        "#,
        r.display()
    ))
    .unwrap();
    Env {
        state: Arc::new(AppState::new(cfg, I18n::builtin())),
        _tmp: tmp,
    }
}

impl Env {
    fn act_as(&self, p: Option<Principal>) {
        *self.state.test_principal.lock().unwrap() = p;
    }

    async fn send(&self, method: &str, uri: &str, peer: &str, headers: &[(&str, &str)]) -> Res {
        let mut b = Request::builder().method(method).uri(uri);
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        let mut req = b.body(Body::empty()).unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::new(peer.parse().unwrap(), 40000)));
        let res = router(self.state.clone()).oneshot(req).await.unwrap();
        let status = res.status();
        let headers = res.headers().clone();
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec();
        Res {
            status,
            headers,
            body,
        }
    }

    async fn get(&self, uri: &str) -> Res {
        self.send("GET", uri, "127.0.0.1", &[]).await
    }

    async fn get_h(&self, uri: &str, headers: &[(&str, &str)]) -> Res {
        self.send("GET", uri, "127.0.0.1", headers).await
    }
}

fn zip_names(body: &[u8]) -> Vec<String> {
    let archive = zip::ZipArchive::new(Cursor::new(body.to_vec())).unwrap();
    archive
        .file_names()
        .map(|n| n.unwrap().into_owned())
        .collect()
}

#[tokio::test]
async fn anonymous_sees_public_volumes_and_a_sign_in_link() {
    let e = env();
    let r = e.get("/").await;
    assert_eq!(r.status, StatusCode::OK);
    let t = r.text();
    assert!(
        t.contains(">Share<") && !t.contains("Medien") && !t.contains("Backups"),
        "{t}"
    );
    assert!(t.contains("/.cc/login"));
}

#[tokio::test]
async fn anonymous_is_sent_to_sign_in_for_private_volumes() {
    let e = env();
    let r = e.get("/Medien/Urlaub%20Gardasee/?zip").await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(
        r.header("location"),
        "/.cc/login?next=%2FMedien%2FUrlaub%2520Gardasee%2F%3Fzip"
    );
}

#[tokio::test]
async fn admin_listing_hides_dotfiles_until_asked_and_never_shows_internals() {
    let e = env();
    e.act_as(Some(Principal::Admin));
    let t = e.get("/Medien/").await.text();
    assert!(
        t.contains("Urlaub Gardasee") && t.contains("a.txt") && t.contains("Media files"),
        "{t}"
    );
    assert!(
        !t.contains(".git") && !t.contains(".chestcraft-trash") && !t.contains(">escape<"),
        "{t}"
    );
    let t = e.get("/Medien/?hidden=1").await.text();
    assert!(
        t.contains(".git") && !t.contains(".chestcraft-trash"),
        "{t}"
    );
}

#[tokio::test]
async fn non_admins_cant_reach_hidden_paths() {
    let e = env();
    e.act_as(Some(Principal::User {
        role: "nobody".into(),
    }));
    // A user without a role on Share still reads it because it's public, but not dot-paths.
    assert_eq!(e.get("/Share/hello.txt").await.status, StatusCode::OK);
    assert_eq!(e.get("/Medien/").await.status, StatusCode::NOT_FOUND);
    e.act_as(Some(Principal::Admin));
    assert_eq!(e.get("/Medien/.git/").await.status, StatusCode::OK);
}

#[tokio::test]
async fn folders_without_trailing_slash_redirect() {
    let e = env();
    e.act_as(Some(Principal::Admin));
    let r = e.get("/Medien").await;
    assert_eq!(
        (r.status, r.header("location")),
        (StatusCode::PERMANENT_REDIRECT, "/Medien/")
    );
    let r = e.get("/Medien/Urlaub%20Gardasee?sort=size").await;
    assert_eq!(r.header("location"), "/Medien/Urlaub%20Gardasee/?sort=size");
    let r = e.get("/Medien/a.txt/").await;
    assert_eq!(r.header("location"), "/Medien/a.txt");
}

#[tokio::test]
async fn traversal_escapes_and_internals_are_not_found() {
    let e = env();
    e.act_as(Some(Principal::Admin));
    for uri in [
        "/Medien/../outside.txt",
        "/Medien/%2e%2e/outside.txt",
        "/Medien/..%2Foutside.txt",
        "/Medien/escape",
        "/Medien/.chestcraft-trash/",
        "/Nope/",
    ] {
        assert_eq!(e.get(uri).await.status, StatusCode::NOT_FOUND, "{uri}");
    }
    assert_eq!(e.get("/Medien/%ff").await.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn downloads_support_range_etag_and_head() {
    let e = env();
    e.act_as(Some(Principal::Admin));
    let r = e.get("/Medien/a.txt").await;
    assert_eq!(
        (r.status, r.text().as_str()),
        (StatusCode::OK, "0123456789")
    );
    assert_eq!(r.header("accept-ranges"), "bytes");
    assert_eq!(r.header("content-type"), "text/plain; charset=utf-8");
    assert!(r.header("content-disposition").starts_with("inline;"));
    let etag = r.header("etag").to_string();

    let r = e.get_h("/Medien/a.txt", &[("range", "bytes=2-5")]).await;
    assert_eq!(
        (r.status, r.text().as_str()),
        (StatusCode::PARTIAL_CONTENT, "2345")
    );
    assert_eq!(r.header("content-range"), "bytes 2-5/10");
    assert_eq!(
        e.get_h("/Medien/a.txt", &[("if-none-match", &etag)])
            .await
            .status,
        StatusCode::NOT_MODIFIED
    );
    let r = e.get_h("/Medien/a.txt", &[("range", "bytes=20-")]).await;
    assert_eq!(
        (r.status, r.header("content-range")),
        (StatusCode::RANGE_NOT_SATISFIABLE, "bytes */10")
    );
    let r = e
        .get_h(
            "/Medien/a.txt",
            &[("range", "bytes=2-5"), ("if-range", "\"old\"")],
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "stale If-Range sends the whole file"
    );
    assert!(
        e.get("/Medien/a.txt?dl")
            .await
            .header("content-disposition")
            .starts_with("attachment;")
    );

    let r = e.send("HEAD", "/Medien/a.txt", "127.0.0.1", &[]).await;
    assert_eq!(
        (r.status, r.header("content-length"), r.body.len()),
        (StatusCode::OK, "10", 0)
    );
}

#[tokio::test]
async fn html_files_download_sandboxed() {
    let e = env();
    e.act_as(Some(Principal::Admin));
    let r = e.get("/Medien/page.html").await;
    assert!(r.header("content-disposition").starts_with("attachment;"));
    assert!(r.header("content-security-policy").starts_with("sandbox"));
}

#[tokio::test]
async fn zip_of_a_folder_and_of_a_selection() {
    let e = env();
    e.act_as(Some(Principal::Admin));
    let r = e.get("/Medien/?zip").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.header("content-type"), "application/zip");
    assert!(r.header("content-disposition").contains("Medien.zip"));
    assert_eq!(
        zip_names(&r.body),
        [
            "a.txt",
            "page.html",
            "Urlaub Gardasee/",
            "Urlaub Gardasee/IMG_2041.jpg"
        ]
    );

    let r = e.get("/Medien/?zip=1&n=a.txt&n=Urlaub+Gardasee").await;
    assert_eq!(
        zip_names(&r.body),
        ["a.txt", "Urlaub Gardasee/", "Urlaub Gardasee/IMG_2041.jpg"]
    );
    assert_eq!(
        e.get("/Medien/?zip=1&n=..%2Fx").await.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        e.get("/Medien/?zip=1&n=.git").await.status,
        StatusCode::BAD_REQUEST
    );
    let names = zip_names(&e.get("/Medien/?zip=1&hidden=1&n=.git").await.body);
    assert_eq!(names, [".git/"]);
}

#[tokio::test]
async fn local_only_volumes_are_closed_from_the_internet() {
    let e = env();
    e.act_as(Some(Principal::Admin));
    let remote = [("cf-connecting-ip", "203.0.113.5")];
    assert_eq!(
        e.send("GET", "/Backups/", "192.0.2.10", &remote)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert!(
        !e.send("GET", "/", "192.0.2.10", &remote)
            .await
            .text()
            .contains("Backups")
    );
    assert_eq!(e.get("/Backups/").await.status, StatusCode::OK);
    assert!(e.get("/").await.text().contains("Backups"));
    // LAN through the reverse proxy is local.
    let lan = [("x-forwarded-for", "192.168.1.20")];
    assert_eq!(
        e.send("GET", "/Backups/", "192.0.2.11", &lan).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn every_response_carries_the_security_headers() {
    let e = env();
    let r = e.get("/").await;
    assert_eq!(r.header("x-content-type-options"), "nosniff");
    assert_eq!(r.header("referrer-policy"), "no-referrer");
    assert_eq!(r.header("x-robots-tag"), "noindex, nofollow");
    assert_eq!(r.header("cache-control"), "private, no-cache");
    assert!(
        r.header("content-security-policy")
            .contains("default-src 'self'")
    );
    assert_eq!(r.header("x-frame-options"), "DENY");
    let r = e.get("/Share/hello.txt").await;
    assert_eq!(r.header("cache-control"), "private, no-cache");
    assert_eq!(r.header("content-security-policy"), "");
}

#[tokio::test]
async fn robots_health_assets_and_methods() {
    let e = env();
    assert!(e.get("/robots.txt").await.text().contains("Disallow: /"));
    assert_eq!(e.get("/healthz").await.text(), "ok\n");
    let r = e.get("/.cc/assets/app.css").await;
    assert_eq!(
        (r.status, r.header("content-type")),
        (StatusCode::OK, "text/css; charset=utf-8")
    );
    let versioned = assets::url("app.css");
    assert!(
        e.get(&versioned)
            .await
            .header("cache-control")
            .contains("immutable")
    );
    assert_eq!(
        e.get("/.cc/assets/nope.css").await.status,
        StatusCode::NOT_FOUND
    );
    e.act_as(Some(Principal::Admin));
    let r = e.send("POST", "/Medien/", "127.0.0.1", &[]).await;
    assert_eq!(
        (r.status, r.header("allow")),
        (StatusCode::METHOD_NOT_ALLOWED, "GET, HEAD")
    );
}

#[tokio::test]
async fn anonymous_downloads_are_limited_per_ip() {
    let e = env();
    let ip = "127.0.0.1".parse().unwrap();
    let held: Vec<_> = (0..4)
        .map(|_| e.state.limiter.try_acquire(ip, 4).unwrap())
        .collect();
    let r = e.get("/Share/hello.txt").await;
    assert_eq!(
        (r.status, r.header("retry-after")),
        (StatusCode::TOO_MANY_REQUESTS, "10")
    );
    drop(held);
    let r = e.get("/Share/hello.txt").await;
    assert_eq!((r.status, r.text().as_str()), (StatusCode::OK, "hello"));
    assert_eq!(
        e.state.limiter.active(ip),
        0,
        "the slot is released when the body is done"
    );
}

#[tokio::test]
async fn language_follows_cookie_then_browser() {
    let e = env();
    assert!(
        e.get_h("/", &[("accept-language", "de-DE,de;q=0.9")])
            .await
            .text()
            .contains("Öffentliche Ordner")
    );
    let t = e
        .get_h(
            "/",
            &[("accept-language", "de"), ("cookie", "x=1; cc_lang=en")],
        )
        .await
        .text();
    assert!(
        t.contains("Public folders") && t.contains("lang=\"en\""),
        "{t}"
    );
    let t = e.get_h("/", &[("cookie", "cc_theme=dark")]).await.text();
    assert!(t.contains("data-theme=\"dark\""), "{t}");
}
