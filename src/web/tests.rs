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
    std::fs::create_dir_all(r.join("eingang")).unwrap();
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
        [volumes.Eingang]
        path = "{0}/eingang"
        stage_folder_uploads = true
        trash_days = 1
        [uploads]
        chunk_mib = 1
        [roles.familie]
        Medien = "rwmd"
        Eingang = "rw"
        [roles.leser]
        Medien = "r"
        "#,
        r.display()
    ))
    .unwrap();
    let db = open_db(&cfg).unwrap();
    Env {
        state: Arc::new(AppState::new(cfg, I18n::builtin(), db)),
        _tmp: tmp,
    }
}

impl Env {
    fn act_as(&self, p: Option<Principal>) {
        *self.state.test_principal.lock().unwrap() = p;
    }

    async fn send(&self, method: &str, uri: &str, peer: &str, headers: &[(&str, &str)]) -> Res {
        self.send_body(method, uri, peer, headers, String::new())
            .await
    }

    async fn send_body(
        &self,
        method: &str,
        uri: &str,
        peer: &str,
        headers: &[(&str, &str)],
        body: String,
    ) -> Res {
        let mut b = Request::builder().method(method).uri(uri);
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        let mut req = b.body(Body::from(body)).unwrap();
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

fn admin() -> Principal {
    Principal::Admin {
        name: "admin".into(),
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
    e.act_as(Some(admin()));
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
        name: "jana".into(),
        role: "nobody".into(),
    }));
    // A user without a role on Share still reads it because it's public, but not dot-paths.
    assert_eq!(e.get("/Share/hello.txt").await.status, StatusCode::OK);
    assert_eq!(e.get("/Medien/").await.status, StatusCode::NOT_FOUND);
    e.act_as(Some(admin()));
    assert_eq!(e.get("/Medien/.git/").await.status, StatusCode::OK);
}

#[tokio::test]
async fn folders_without_trailing_slash_redirect() {
    let e = env();
    e.act_as(Some(admin()));
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
    e.act_as(Some(admin()));
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
    e.act_as(Some(admin()));
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
    e.act_as(Some(admin()));
    let r = e.get("/Medien/page.html").await;
    assert!(r.header("content-disposition").starts_with("attachment;"));
    assert!(r.header("content-security-policy").starts_with("sandbox"));
}

#[tokio::test]
async fn zip_of_a_folder_and_of_a_selection() {
    let e = env();
    e.act_as(Some(admin()));
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
    e.act_as(Some(admin()));
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
    e.act_as(Some(admin()));
    let r = e.send("PATCH", "/Medien/", "127.0.0.1", &[]).await;
    assert_eq!(
        (r.status, r.header("allow")),
        (StatusCode::METHOD_NOT_ALLOWED, "GET, HEAD, POST")
    );
    let r = e.send("POST", "/Medien/a.txt", "127.0.0.1", &[]).await;
    assert_eq!(
        r.status,
        StatusCode::METHOD_NOT_ALLOWED,
        "forms post to folders"
    );
    let r = e
        .send_body(
            "POST",
            "/Medien/",
            "127.0.0.1",
            &[FORM],
            "op=mkdir&name=x".into(),
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::FORBIDDEN,
        "changes need a session and its token"
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

// --- accounts and sessions ---------------------------------------------------------------

use crate::auth::password;
use crate::db::{Session, User};

const FORM: (&str, &str) = ("content-type", "application/x-www-form-urlencoded");

fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| {
            format!(
                "{k}={}",
                percent_encoding::utf8_percent_encode(v, percent_encoding::NON_ALPHANUMERIC)
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn csrf_of(page: &str) -> String {
    let i = page.find("name=\"csrf\" value=\"").expect("csrf field") + 19;
    page[i..i + 32].to_string()
}

fn cookie_of(r: &Res) -> String {
    r.header("set-cookie")
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

impl Env {
    fn add_user(&self, name: &str, role: &str, pw: &str, must_change: bool) {
        self.state
            .db
            .put_user(&User {
                name: name.into(),
                role: role.into(),
                password_hash: password::hash(pw),
                must_change_password: must_change,
                disabled: false,
                created: 0,
                last_login: None,
                settings: Default::default(),
                known_remote_ips: vec![],
            })
            .unwrap();
    }

    async fn login_from(
        &self,
        peer: &str,
        headers: &[(&str, &str)],
        user: &str,
        pw: &str,
        next: &str,
    ) -> Res {
        let mut h = vec![FORM];
        h.extend_from_slice(headers);
        let body = form(&[("username", user), ("password", pw), ("next", next)]);
        self.send_body("POST", "/.cc/login", peer, &h, body).await
    }

    async fn login(&self, user: &str, pw: &str) -> Res {
        self.login_from("127.0.0.1", &[], user, pw, "/").await
    }
}

const PW: &str = "richtig-lang-123";

#[tokio::test]
async fn sign_in_reach_private_volume_and_sign_out() {
    let e = env();
    e.add_user("Tibor", "admin", PW, false);
    let r = e
        .login_from("127.0.0.1", &[], "tibor", PW, "/Medien/")
        .await;
    assert_eq!(
        (r.status, r.header("location")),
        (StatusCode::SEE_OTHER, "/Medien/")
    );
    let set = r.header("set-cookie");
    assert!(
        set.contains("HttpOnly") && set.contains("SameSite=Lax") && !set.contains("Secure"),
        "{set}"
    );
    let cookie = cookie_of(&r);

    let page = e.get_h("/Medien/", &[("cookie", &cookie)]).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.text().contains("Signed in as Tibor"));
    let csrf = csrf_of(&page.text());

    let out = e
        .send_body(
            "POST",
            "/.cc/logout",
            "127.0.0.1",
            &[FORM, ("cookie", &cookie)],
            form(&[("csrf", &csrf)]),
        )
        .await;
    assert_eq!(
        (out.status, out.header("location")),
        (StatusCode::SEE_OTHER, "/")
    );
    assert!(out.header("set-cookie").contains("Max-Age=0"));
    let after = e.get_h("/Medien/", &[("cookie", &cookie)]).await;
    assert_eq!(
        after.status,
        StatusCode::SEE_OTHER,
        "the old cookie no longer works"
    );

    let actions = e.audited(&["login", "logout"]).await;
    assert!(
        actions.contains(&"login".into()) && actions.contains(&"logout".into()),
        "{actions:?}"
    );
}

#[tokio::test]
async fn wrong_passwords_slow_down_that_address_only() {
    let e = env();
    e.add_user("jana", "familie", PW, false);
    for _ in 0..4 {
        let r = e.login("jana", "falsch-falsch").await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
        assert!(r.text().contains("Username or password is wrong."));
    }
    let r = e.login("jana", "falsch-falsch").await;
    assert!(r.text().contains("Try again in 1 min"), "{}", r.text());
    let r = e.login("jana", PW).await;
    assert_eq!(
        r.status,
        StatusCode::TOO_MANY_REQUESTS,
        "even the right password waits"
    );
    // Someone else (another address through the LAN proxy) still gets in.
    let r = e
        .login_from(
            "192.0.2.11",
            &[("x-forwarded-for", "192.168.1.30")],
            "jana",
            PW,
            "/",
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn unknown_and_disabled_accounts_look_like_a_wrong_password() {
    let e = env();
    e.add_user("mika", "familie", PW, false);
    let mut u = e.state.db.user("mika").unwrap().unwrap();
    u.disabled = true;
    e.state.db.put_user(&u).unwrap();
    for user in ["nobody", "mika"] {
        let r = e.login(user, PW).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{user}");
        assert!(r.text().contains("Username or password is wrong."));
    }
}

#[tokio::test]
async fn first_sign_in_forces_a_new_password() {
    let e = env();
    e.add_user("Markus", "gast", "einmal-passwort", true);
    let r = e.login("markus", "einmal-passwort").await;
    assert_eq!(r.header("location"), "/.cc/password");
    let cookie = cookie_of(&r);
    let blocked = e.get_h("/", &[("cookie", &cookie)]).await;
    assert_eq!(
        (blocked.status, blocked.header("location")),
        (StatusCode::SEE_OTHER, "/.cc/password")
    );

    let page = e
        .get_h("/.cc/password", &[("cookie", &cookie)])
        .await
        .text();
    assert!(
        !page.contains("name=\"current\""),
        "no current password on the forced change"
    );
    let csrf = csrf_of(&page);
    let post = |new: &'static str, repeat: &'static str| {
        let body = form(&[("new", new), ("repeat", repeat), ("csrf", &csrf)]);
        let h = [FORM, ("cookie", cookie.as_str())];
        let e = &e;
        async move {
            e.send_body("POST", "/.cc/password", "127.0.0.1", &h, body)
                .await
        }
    };
    assert!(
        post("kurz", "kurz")
            .await
            .text()
            .contains("at least 10 characters")
    );
    assert!(
        post("lang-genug-1", "lang-genug-2")
            .await
            .text()
            .contains("don't match")
    );
    assert!(
        post("einmal-passwort", "einmal-passwort")
            .await
            .text()
            .contains("don't use here yet")
    );
    let ok = post("mein-neues-passwort", "mein-neues-passwort").await;
    assert_eq!(
        (ok.status, ok.header("location")),
        (StatusCode::SEE_OTHER, "/")
    );
    assert_eq!(
        e.get_h("/", &[("cookie", &cookie)]).await.status,
        StatusCode::OK
    );
    assert!(e.login("markus", "mein-neues-passwort").await.status == StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn csrf_and_origin_are_enforced() {
    let e = env();
    e.add_user("Tibor", "admin", PW, false);
    let cookie = cookie_of(&e.login("tibor", PW).await);
    let r = e
        .send_body(
            "POST",
            "/.cc/logout",
            "127.0.0.1",
            &[FORM, ("cookie", &cookie)],
            form(&[("csrf", "nope")]),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = e
        .login_from(
            "127.0.0.1",
            &[
                ("origin", "https://evil.example"),
                ("host", "files.example.org"),
            ],
            "tibor",
            PW,
            "/",
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = e
        .login_from("127.0.0.1", &[], "tibor", PW, "//evil.example")
        .await;
    assert_eq!(
        r.header("location"),
        "/",
        "foreign redirect targets are dropped"
    );
}

#[tokio::test]
async fn remote_admin_sessions_end_after_twelve_hours() {
    let e = env();
    e.add_user("Tibor", "admin", PW, false);
    let now = super::auth::now();
    let old = crate::auth::session::create(
        "tibor",
        "203.0.113.5".parse().unwrap(),
        true,
        now - 13 * 3600,
    );
    e.state
        .db
        .put_session(
            &old.id_hash,
            &Session {
                last_seen: now,
                ..old.session.clone()
            },
            true,
        )
        .unwrap();
    let cookie = format!("cc_session={}", old.cookie_value);
    let remote = [
        ("cf-connecting-ip", "203.0.113.5"),
        ("cookie", cookie.as_str()),
    ];
    let r = e.send("GET", "/Medien/", "192.0.2.10", &remote).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "treated as signed out");
    assert!(
        e.state.db.session(&old.id_hash).unwrap().is_none(),
        "and the session is gone"
    );
}

#[tokio::test]
async fn cookies_are_secure_behind_https() {
    let e = env();
    e.add_user("Tibor", "admin", PW, false);
    let r = e
        .login_from(
            "192.0.2.10",
            &[
                ("cf-connecting-ip", "203.0.113.9"),
                ("x-forwarded-proto", "https"),
            ],
            "tibor",
            PW,
            "/",
        )
        .await;
    let set = r.header("set-cookie");
    assert!(
        set.contains("; Secure") && set.contains(&format!("Max-Age={}", 12 * 3600)),
        "{set}"
    );
    let user = e.state.db.user("tibor").unwrap().unwrap();
    assert_eq!(user.known_remote_ips, ["203.0.113.9"]);
}

// --- uploads, folders and the trash --------------------------------------------------------

const JSON: (&str, &str) = ("content-type", "application/json");
const MIB: usize = 1 << 20;

struct Login {
    cookie: String,
    csrf: String,
}

impl Env {
    fn root(&self) -> &std::path::Path {
        self._tmp.path()
    }

    async fn session(&self, name: &str, role: &str) -> Login {
        self.add_user(name, role, PW, false);
        let cookie = cookie_of(&self.login(name, PW).await);
        let page = self.get_h("/", &[("cookie", &cookie)]).await.text();
        Login {
            csrf: csrf_of(&page),
            cookie,
        }
    }

    async fn call(&self, s: &Login, method: &str, uri: &str, body: String) -> Res {
        self.send_body(
            method,
            uri,
            "127.0.0.1",
            &[JSON, ("cookie", &s.cookie), ("x-csrf-token", &s.csrf)],
            body,
        )
        .await
    }

    async fn put_chunk(&self, s: &Login, id: &str, n: u32, data: &[u8]) -> Res {
        let mut b = Request::builder()
            .method("PUT")
            .uri(format!("/.cc/upload/{id}/chunk/{n}"))
            .header("cookie", &s.cookie)
            .header("x-csrf-token", &s.csrf)
            .header("content-length", data.len());
        b = b.header("content-type", "application/octet-stream");
        let mut req = b.body(Body::from(data.to_vec())).unwrap();
        req.extensions_mut().insert(ConnectInfo(SocketAddr::new(
            "127.0.0.1".parse().unwrap(),
            40000,
        )));
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

    /// Upload `data` completely; returns the response of `done`.
    async fn upload(
        &self,
        s: &Login,
        vol: &str,
        dir: &str,
        name: &str,
        data: &[u8],
        clash: &str,
    ) -> Res {
        let r = self
            .start_upload(s, vol, dir, name, data.len(), clash, None)
            .await;
        if r.status != StatusCode::CREATED {
            return r;
        }
        let st: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
        let id = st["id"].as_str().unwrap().to_string();
        let chunk = st["chunk"].as_u64().unwrap() as usize;
        for (n, part) in data.chunks(chunk).enumerate().rev() {
            let c = self.put_chunk(s, &id, n as u32, part).await;
            assert_eq!(c.status, StatusCode::OK, "{}", c.text());
        }
        self.call(s, "POST", &format!("/.cc/upload/{id}/done"), String::new())
            .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn start_upload(
        &self,
        s: &Login,
        vol: &str,
        dir: &str,
        name: &str,
        size: usize,
        clash: &str,
        batch: Option<&str>,
    ) -> Res {
        let body = serde_json::json!({
            "vol": vol, "dir": dir, "name": name, "size": size,
            "mtime": 1_700_000_000_000i64, "clash": clash, "batch": batch,
        });
        self.call(s, "POST", "/.cc/upload", body.to_string()).await
    }

    async fn form_post(&self, s: &Login, uri: &str, pairs: &[(&str, &str)]) -> Res {
        let mut all = vec![("csrf", s.csrf.as_str())];
        all.extend_from_slice(pairs);
        self.send_body(
            "POST",
            uri,
            "127.0.0.1",
            &[FORM, ("cookie", &s.cookie)],
            form(&all),
        )
        .await
    }

    /// Audit actions, once `expected` are all written (audit writes don't block requests).
    async fn audited(&self, expected: &[&str]) -> Vec<String> {
        let mut actions = Vec::new();
        for _ in 0..100 {
            actions = self
                .state
                .db
                .recent_audit(50)
                .unwrap()
                .into_iter()
                .map(|a| a.action)
                .collect::<Vec<String>>();
            if expected.iter().all(|x| actions.iter().any(|a| a == x)) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        actions
    }
}

fn json_of(r: &Res) -> serde_json::Value {
    serde_json::from_slice(&r.body).unwrap_or_else(|_| panic!("not JSON: {}", r.text()))
}

#[tokio::test]
async fn chunked_upload_lands_with_its_mtime() {
    let e = env();
    let s = e.session("Tibor", "admin").await;
    let data: Vec<u8> = (0..(2 * MIB + 123)).map(|i| (i % 251) as u8).collect();
    let r = e
        .upload(
            &s,
            "Medien",
            "Urlaub Gardasee",
            "Tag 1/Video.mp4",
            &data,
            "ask",
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        json_of(&r),
        serde_json::json!({"path": "Tag 1/Video.mp4", "renamed": false})
    );
    let path = e.root().join("medien/Urlaub Gardasee/Tag 1/Video.mp4");
    assert_eq!(std::fs::read(&path).unwrap(), data);
    use std::os::unix::fs::MetadataExt;
    assert_eq!(std::fs::metadata(&path).unwrap().mtime(), 1_700_000_000);
    assert!(e.audited(&["upload"]).await.contains(&"upload".to_string()));
    assert!(e.state.db.uploads().unwrap().is_empty());
}

#[tokio::test]
async fn upload_status_and_resume() {
    let e = env();
    let s = e.session("Tibor", "admin").await;
    let data = vec![9u8; MIB + 10];
    let r = e
        .start_upload(&s, "Medien", "", "resume.bin", data.len(), "ask", None)
        .await;
    let id = json_of(&r)["id"].as_str().unwrap().to_string();
    assert_eq!(json_of(&r)["chunks"], 2);
    assert_eq!(
        e.put_chunk(&s, &id, 1, &data[MIB..]).await.status,
        StatusCode::OK
    );
    let st = json_of(
        &e.call(&s, "GET", &format!("/.cc/upload/{id}"), String::new())
            .await,
    );
    assert_eq!(st["have"], serde_json::json!([1]));
    // Wrong length and finishing early are refused.
    assert_eq!(
        e.put_chunk(&s, &id, 0, b"short").await.status,
        StatusCode::BAD_REQUEST
    );
    let early = e
        .call(&s, "POST", &format!("/.cc/upload/{id}/done"), String::new())
        .await;
    assert_eq!(
        (early.status, json_of(&early)["error"].as_str()),
        (StatusCode::CONFLICT, Some("incomplete"))
    );
    assert_eq!(
        e.put_chunk(&s, &id, 0, &data[..MIB]).await.status,
        StatusCode::OK
    );
    let done = e
        .call(&s, "POST", &format!("/.cc/upload/{id}/done"), String::new())
        .await;
    assert_eq!(done.status, StatusCode::OK);
    assert_eq!(
        std::fs::read(e.root().join("medien/resume.bin")).unwrap(),
        data
    );
}

#[tokio::test]
async fn name_clash_answers_and_replace_goes_to_the_trash() {
    let e = env();
    let s = e.session("Tibor", "admin").await;
    let r = e
        .start_upload(&s, "Medien", "", "a.txt", 3, "ask", None)
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    let j = json_of(&r);
    assert_eq!(j["clash"]["name"], "a.txt");
    assert_eq!(j["clash"]["size"], 10);
    assert_eq!(j["clash"]["can_replace"], true);
    assert!(j["message"].as_str().unwrap().contains("a.txt"));

    let r = e.upload(&s, "Medien", "", "a.txt", b"new", "rename").await;
    assert_eq!(json_of(&r)["path"], "a (1).txt");
    let r = e.upload(&s, "Medien", "", "a.txt", b"newer", "skip").await;
    assert_eq!(json_of(&r)["skipped"], true);
    let r = e
        .upload(&s, "Medien", "", "a.txt", b"newest", "replace")
        .await;
    assert_eq!(json_of(&r)["renamed"], false);
    assert_eq!(
        std::fs::read(e.root().join("medien/a.txt")).unwrap(),
        b"newest"
    );
    let trash = e.state.db.trash_items().unwrap();
    assert_eq!(trash.len(), 1);
    assert_eq!(
        (trash[0].path.as_str(), trash[0].reason.as_str()),
        ("a.txt", "replaced")
    );
}

#[tokio::test]
async fn upload_rights_are_checked() {
    let e = env();
    let leser = e.session("lena", "leser").await;
    let r = e
        .start_upload(&leser, "Medien", "", "x.txt", 1, "ask", None)
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // Users without delete rights can't replace.
    let fam = e.session("jana", "familie").await;
    let r = e
        .start_upload(&fam, "Eingang", "", "x.txt", 1, "replace", None)
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = e
        .start_upload(&fam, "Backups", "", "x.txt", 1, "ask", None)
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = e
        .start_upload(&fam, "Medien", "", ".hidden", 1, "ask", None)
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "dot-names are admin-only");
    let r = e
        .start_upload(&fam, "Medien", "", "a/.chestcraft-trash/x", 1, "ask", None)
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = e
        .start_upload(&fam, "Medien", "", "../x", 1, "ask", None)
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    // Someone else's upload doesn't exist for you.
    let r = e
        .start_upload(&fam, "Medien", "", "mine.txt", 1, "ask", None)
        .await;
    let id = json_of(&r)["id"].as_str().unwrap().to_string();
    let admin = e.session("Tibor", "admin").await;
    assert_eq!(
        e.put_chunk(&admin, &id, 0, b"x").await.status,
        StatusCode::NOT_FOUND
    );
    // No token, no session.
    let bad = Login {
        cookie: fam.cookie.clone(),
        csrf: "0".repeat(32),
    };
    assert_eq!(
        e.put_chunk(&bad, &id, 0, b"x").await.status,
        StatusCode::FORBIDDEN
    );
    let r = e
        .send_body("POST", "/.cc/upload", "127.0.0.1", &[JSON], "{}".into())
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // Cancel removes the data.
    let r = e
        .call(&fam, "DELETE", &format!("/.cc/upload/{id}"), String::new())
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(
        std::fs::read_dir(e.root().join("medien/.chestcraft-uploads"))
            .unwrap()
            .count(),
        0
    );
}

#[tokio::test]
async fn remote_uploads_to_a_home_only_volume_are_refused() {
    let e = env();
    let s = e.session("Tibor", "admin").await;
    let body = serde_json::json!({"vol": "Backups", "dir": "", "name": "x", "size": 1});
    let r = e
        .send_body(
            "POST",
            "/.cc/upload",
            "192.0.2.10",
            &[
                JSON,
                ("cookie", &s.cookie),
                ("x-csrf-token", &s.csrf),
                ("cf-connecting-ip", "203.0.113.5"),
            ],
            body.to_string(),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn staged_folder_upload_appears_when_the_batch_is_done() {
    let e = env();
    let s = e.session("jana", "familie").await;
    let b = json_of(
        &e.call(
            &s,
            "POST",
            "/.cc/batch",
            r#"{"vol":"Eingang","dir":""}"#.into(),
        )
        .await,
    );
    assert_eq!(b["staged"], true);
    let batch = b["id"].as_str().unwrap().to_string();
    for name in ["Scan/a.pdf", "Scan/b/c.pdf"] {
        let r = e
            .start_upload(&s, "Eingang", "", name, 2, "ask", Some(&batch))
            .await;
        let id = json_of(&r)["id"].as_str().unwrap().to_string();
        e.put_chunk(&s, &id, 0, b"pd").await;
        let r = e
            .call(&s, "POST", &format!("/.cc/upload/{id}/done"), String::new())
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        assert!(!e.root().join("eingang/Scan").exists());
    }
    let r = e
        .call(
            &s,
            "POST",
            &format!("/.cc/batch/{batch}/done"),
            String::new(),
        )
        .await;
    assert_eq!(json_of(&r)["moved"], serde_json::json!([["Scan", "Scan"]]));
    assert!(e.root().join("eingang/Scan/b/c.pdf").is_file());
    assert!(
        e.audited(&["upload_folder"])
            .await
            .contains(&"upload_folder".to_string())
    );
}

#[tokio::test]
async fn listing_offers_changes_only_to_those_who_may() {
    let e = env();
    let fam = e.session("jana", "familie").await;
    let t = e.get_h("/Medien/", &[("cookie", &fam.cookie)]).await.text();
    assert!(
        t.contains("data-js=\"upload\"") && t.contains("value=\"mkdir\""),
        "{t}"
    );
    assert!(t.contains("value=\"delete\"") && t.contains("upload.js"));
    let leser = e.session("lena", "leser").await;
    let t = e
        .get_h("/Medien/", &[("cookie", &leser.cookie)])
        .await
        .text();
    assert!(!t.contains("data-js=\"upload\"") && !t.contains("value=\"delete\""));
    assert!(t.contains("value=\"zip\""));
    let t = e.get("/Share/").await.text();
    assert!(!t.contains("upload.js") && !t.contains("name=\"csrf\""));
}

#[tokio::test]
async fn new_folder_delete_restore_and_empty() {
    let e = env();
    let fam = e.session("jana", "familie").await;
    let r = e
        .form_post(&fam, "/Medien/", &[("op", "mkdir"), ("name", " Neu ")])
        .await;
    assert_eq!(
        (r.status, r.header("location")),
        (StatusCode::SEE_OTHER, "/Medien/")
    );
    assert!(e.root().join("medien/Neu").is_dir());
    let r = e
        .form_post(&fam, "/Medien/", &[("op", "mkdir"), ("name", "Neu")])
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    let r = e
        .form_post(
            &fam,
            "/Medien/",
            &[("op", "mkdir"), ("name", ".chestcraft-x")],
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    // Zip of a selection by POST works without a session too.
    let r = e
        .send_body(
            "POST",
            "/Share/",
            "127.0.0.1",
            &[FORM],
            "op=zip&n=hello.txt".into(),
        )
        .await;
    assert_eq!(zip_names(&r.body), ["hello.txt"]);

    let r = e
        .form_post(
            &fam,
            "/Medien/",
            &[
                ("op", "delete"),
                ("n", "Neu"),
                ("n", "a.txt"),
                ("n", "nope"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "one of three failed");
    assert!(r.text().contains("nope"));
    assert!(!e.root().join("medien/Neu").exists() && !e.root().join("medien/a.txt").exists());

    let page = e
        .get_h("/.cc/trash", &[("cookie", &fam.cookie)])
        .await
        .text();
    assert!(
        page.contains("a.txt") && page.contains("Neu") && page.contains("in 30 days"),
        "{page}"
    );
    // Another user doesn't see jana's trash; the admin sees everything.
    let leser = e.session("lena", "leser").await;
    let other = e
        .get_h("/.cc/trash", &[("cookie", &leser.cookie)])
        .await
        .text();
    assert!(!other.contains("a.txt"), "{other}");
    let admin = e.session("Tibor", "admin").await;
    let all = e
        .get_h("/.cc/trash", &[("cookie", &admin.cookie)])
        .await
        .text();
    assert!(all.contains("a.txt") && all.contains(">jana<") && all.contains("/.cc/trash/empty"));

    let item = e
        .state
        .db
        .trash_items()
        .unwrap()
        .into_iter()
        .find(|i| i.path == "a.txt")
        .unwrap();
    let r = e
        .form_post(&leser, "/.cc/trash/restore", &[("id", &item.id)])
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "not lena's item");
    std::fs::write(e.root().join("medien/a.txt"), "taken").unwrap();
    let r = e
        .form_post(&fam, "/.cc/trash/restore", &[("id", &item.id)])
        .await;
    assert_eq!(
        (r.status, r.header("location")),
        (StatusCode::SEE_OTHER, "/.cc/trash")
    );
    assert_eq!(
        std::fs::read(e.root().join("medien/a (restored).txt")).unwrap(),
        b"0123456789"
    );

    // Only the admin empties, and only with the box ticked.
    let r = e
        .form_post(
            &fam,
            "/.cc/trash/empty",
            &[("vol", "Medien"), ("confirm", "1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = e
        .form_post(&admin, "/.cc/trash/empty", &[("vol", "Medien")])
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = e
        .form_post(
            &admin,
            "/.cc/trash/empty",
            &[("vol", "Medien"), ("confirm", "1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert!(e.state.db.trash_items().unwrap().is_empty());
    assert_eq!(
        std::fs::read_dir(e.root().join("medien/.chestcraft-trash"))
            .unwrap()
            .count(),
        0
    );
    let expected = [
        "mkdir",
        "trash",
        "trash_restored",
        "trash_purged",
        "trash_emptied",
    ];
    let actions = e.audited(&expected).await;
    for a in expected {
        assert!(actions.contains(&a.to_string()), "{a}: {actions:?}");
    }
}

#[tokio::test]
async fn deleting_needs_the_right_and_a_token() {
    let e = env();
    let leser = e.session("lena", "leser").await;
    let r = e
        .form_post(&leser, "/Medien/", &[("op", "delete"), ("n", "a.txt")])
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert!(r.text().contains("can't delete"), "{}", r.text());
    let fam = e.session("jana", "familie").await;
    let r = e
        .send_body(
            "POST",
            "/Medien/",
            "127.0.0.1",
            &[FORM, ("cookie", &fam.cookie)],
            "op=delete&n=a.txt&csrf=wrong".into(),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(e.root().join("medien/a.txt").exists());
}

#[tokio::test]
async fn daily_cleanup_purges_what_is_due() {
    let e = env();
    let fam = e.session("jana", "familie").await;
    std::fs::write(e.root().join("eingang/alt.txt"), "x").unwrap();
    e.state
        .uploads
        .create_batch(
            e.state.volumes.get("Eingang").unwrap(),
            &crate::fs::path::RelPath::root(),
            "user:jana",
            0,
        )
        .unwrap();
    let vol = e.state.volumes.get("Eingang").unwrap().clone();
    let item = crate::fs::trash::delete(
        &e.state.db,
        &vol,
        &crate::fs::path::RelPath::parse("alt.txt").unwrap(),
        "user:jana",
        "delete",
        0,
    )
    .unwrap();
    super::jobs::run(e.state.clone()).await;
    assert!(
        e.state.db.trash_item(&item.id).unwrap().is_none(),
        "trash_days = 1 has passed"
    );
    assert!(
        e.state.db.batches().unwrap().is_empty(),
        "the abandoned batch is closed"
    );
    let _ = fam;
}
