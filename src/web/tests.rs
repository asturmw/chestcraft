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
        [roles.teiler]
        Medien = "rwmds"
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
        self.put_chunk_at("/.cc", s, id, n, data).await
    }

    async fn put_chunk_at(&self, base: &str, s: &Login, id: &str, n: u32, data: &[u8]) -> Res {
        let mut b = Request::builder()
            .method("PUT")
            .uri(format!("{base}/upload/{id}/chunk/{n}"))
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
        self.upload_at("/.cc", s, vol, dir, name, data, clash).await
    }

    /// Like `upload`, against the API at `base` (`/s/<id>/.cc` for share visitors).
    #[allow(clippy::too_many_arguments)]
    async fn upload_at(
        &self,
        base: &str,
        s: &Login,
        vol: &str,
        dir: &str,
        name: &str,
        data: &[u8],
        clash: &str,
    ) -> Res {
        let body = serde_json::json!({
            "vol": vol, "dir": dir, "name": name, "size": data.len(),
            "mtime": 1_700_000_000_000i64, "clash": clash,
        });
        let r = self
            .call(s, "POST", &format!("{base}/upload"), body.to_string())
            .await;
        if r.status != StatusCode::CREATED {
            return r;
        }
        let st: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
        let id = st["id"].as_str().unwrap().to_string();
        let chunk = st["chunk"].as_u64().unwrap() as usize;
        for (n, part) in data.chunks(chunk).enumerate().rev() {
            let c = self.put_chunk_at(base, s, &id, n as u32, part).await;
            assert_eq!(c.status, StatusCode::OK, "{}", c.text());
        }
        self.call(
            s,
            "POST",
            &format!("{base}/upload/{id}/done"),
            String::new(),
        )
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

// ---------------------------------------------------------------------------------------
// WebDAV

fn base64(data: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn basic(user: &str, secret: &str) -> String {
    format!("Basic {}", base64(format!("{user}:{secret}").as_bytes()))
}

const PROPFIND_RCLONE: &str = r#"<?xml version="1.0"?>
<d:propfind xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns">
 <d:prop><d:displayname/><d:getlastmodified/><d:getcontentlength/><d:resourcetype/>
 <d:getcontenttype/><oc:checksums/><oc:permissions/></d:prop>
</d:propfind>"#;

impl Env {
    /// An account with an app password; returns the `Authorization` value.
    fn dav_user(&self, name: &str, role: &str) -> String {
        self.add_user(name, role, PW, false);
        let (secret, _) =
            crate::auth::apppass::create(&self.state.db, name, "laptop rclone", 0).unwrap();
        basic(name, &secret)
    }

    async fn dav(
        &self,
        auth: &str,
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Res {
        let mut h = vec![("authorization", auth)];
        h.extend_from_slice(headers);
        self.send_body(method, uri, "127.0.0.1", &h, body.to_string())
            .await
    }

    async fn propfind(&self, auth: &str, uri: &str, depth: &str) -> Res {
        self.dav(auth, "PROPFIND", uri, &[("depth", depth)], PROPFIND_RCLONE)
            .await
    }
}

fn hrefs(r: &Res) -> Vec<String> {
    let t = r.text();
    t.split("<D:href>")
        .skip(1)
        .map(|s| s.split("</D:href>").next().unwrap().to_string())
        .collect()
}

fn mtime_of(p: &std::path::Path) -> i64 {
    std::fs::metadata(p)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

#[tokio::test]
async fn webdav_needs_an_app_password_from_the_local_network() {
    let e = env();
    let auth = e.dav_user("Tibor", "admin");
    // Nothing, the web password, a session cookie: all 401 with a Basic challenge.
    let r = e.send("PROPFIND", "/", "127.0.0.1", &[]).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert!(r.header("www-authenticate").starts_with("Basic realm="));
    let r = e
        .dav(&basic("Tibor", PW), "PROPFIND", "/", &[("depth", "0")], "")
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let cookie = cookie_of(&e.login("Tibor", PW).await);
    let r = e
        .send("PROPFIND", "/Medien/", "127.0.0.1", &[("cookie", &cookie)])
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // Another account's name with Tibor's secret.
    e.add_user("jana", "familie", PW, false);
    let (secret, _) = crate::auth::apppass::create(&e.state.db, "Tibor", "phone", 0).unwrap();
    let r = e
        .dav(&basic("jana", &secret), "PROPFIND", "/", &[], "")
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = e
        .dav(&basic("tibor", &secret), "PROPFIND", "/", &[], "")
        .await;
    assert_eq!(r.status, StatusCode::MULTI_STATUS, "user names ignore case");

    assert_eq!(
        e.propfind(&auth, "/", "1").await.status,
        StatusCode::MULTI_STATUS
    );
    // The internet is refused even with the right password, by Cloudflare headers or by
    // the tunnel's address.
    let h = [
        ("authorization", auth.as_str()),
        ("cf-connecting-ip", "203.0.113.5"),
    ];
    let r = e.send("PROPFIND", "/", "192.0.2.10", &h).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = e.send("PROPFIND", "/", "127.0.0.1", &h).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // OPTIONS needs credentials like everything else.
    let r = e.dav(&auth, "OPTIONS", "/Medien/", &[], "").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.header("dav"), "1, 2");
    assert!(r.header("allow").contains("PROPFIND"));
    // GET and HEAD go to the web handler, which takes the same credentials.
    let r = e.dav(&auth, "GET", "/Backups/dump.bin", &[], "").await;
    assert_eq!((r.status, r.text().as_str()), (StatusCode::OK, "x"));
    let actions = e.audited(&["webdav_login_failed"]).await;
    assert!(actions.iter().any(|a| a == "webdav_login_failed"));
}

#[tokio::test]
async fn webdav_sign_in_failures_back_off_and_revoked_passwords_stop_working() {
    let e = env();
    let auth = e.dav_user("jana", "familie");
    let wrong = basic("jana", "aaaaa-bbbbb-ccccc-ddddd-eeeee-fffff");
    for _ in 0..5 {
        let r = e.dav(&wrong, "PROPFIND", "/", &[], "").await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    }
    let r = e.dav(&auth, "PROPFIND", "/", &[], "").await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(!r.header("retry-after").is_empty());
    // Another address isn't slowed down.
    let h = [("authorization", auth.as_str())];
    let r = e.send("PROPFIND", "/", "192.168.1.30", &h).await;
    assert_eq!(r.status, StatusCode::MULTI_STATUS);

    // Revoked → refused. A reset password stops WebDAV too.
    let e = env();
    let auth = e.dav_user("jana", "familie");
    let key = crate::db::User::key("jana");
    let second = crate::auth::apppass::create(&e.state.db, "jana", "phone", 0)
        .unwrap()
        .0;
    crate::auth::apppass::revoke(&e.state.db, &key, "laptop rclone")
        .unwrap()
        .unwrap();
    assert_eq!(
        e.dav(&auth, "PROPFIND", "/", &[], "").await.status,
        StatusCode::UNAUTHORIZED
    );
    let phone = basic("jana", &second);
    assert_eq!(
        e.dav(&phone, "PROPFIND", "/", &[], "").await.status,
        StatusCode::MULTI_STATUS
    );
    let mut u = e.state.db.user("jana").unwrap().unwrap();
    u.must_change_password = true;
    e.state.db.put_user(&u).unwrap();
    assert_eq!(
        e.dav(&phone, "PROPFIND", "/", &[], "").await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn webdav_listings_follow_the_web_rules() {
    let e = env();
    let admin = e.dav_user("Tibor", "admin");
    let jana = e.dav_user("jana", "familie");

    let r = e.propfind(&jana, "/", "1").await;
    assert_eq!(r.status, StatusCode::MULTI_STATUS);
    assert_eq!(hrefs(&r), ["/", "/Eingang/", "/Medien/", "/Share/"]);
    let r = e.propfind(&admin, "/", "1").await;
    assert!(hrefs(&r).contains(&"/Backups/".to_string()));
    assert_eq!(hrefs(&e.propfind(&jana, "/", "0").await), ["/"]);
    // `rclone about` asks the root for its space.
    let q = r#"<?xml version="1.0"?><D:propfind xmlns:D="DAV:"><D:prop><D:quota-available-bytes/><D:quota-used-bytes/></D:prop></D:propfind>"#;
    let t = e
        .dav(&jana, "PROPFIND", "/", &[("depth", "0")], q)
        .await
        .text();
    assert!(
        // (Not just "404": the byte counts may contain those digits.)
        t.contains("<D:quota-available-bytes>") && !t.contains("404 Not Found"),
        "{t}"
    );

    let r = e.propfind(&jana, "/Medien/", "1").await;
    let t = r.text();
    let mut h = hrefs(&r);
    h.sort();
    assert_eq!(
        h,
        [
            "/Medien/",
            "/Medien/Urlaub%20Gardasee/",
            "/Medien/a.txt",
            "/Medien/page.html"
        ]
    );
    assert!(
        t.contains("<D:getcontentlength>10</D:getcontentlength>"),
        "{t}"
    );
    assert!(
        t.contains("<D:resourcetype><D:collection/></D:resourcetype>"),
        "{t}"
    );
    // Unknown properties are listed as missing after the found ones.
    let found = t.find("HTTP/1.1 200 OK").unwrap();
    let missing = t.find("HTTP/1.1 404 Not Found").unwrap();
    assert!(found < missing && t.contains("checksums"), "{t}");

    // The admin also sees dot-names; nobody sees chestcraft's own folders.
    let h = hrefs(&e.propfind(&admin, "/Medien/", "1").await);
    assert!(h.contains(&"/Medien/.git/".to_string()), "{h:?}");
    assert!(!h.iter().any(|x| x.contains("chestcraft")), "{h:?}");
    assert_eq!(
        e.propfind(&jana, "/Medien/.git/", "0").await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        e.propfind(&admin, "/Medien/.chestcraft-trash/", "0")
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    // No role on Backups → as if it didn't exist. Traversal and escapes too.
    for uri in [
        "/Backups/",
        "/Medien/escape",
        "/Medien/%2e%2e/outside.txt",
        "/Nope/",
    ] {
        assert_eq!(
            e.propfind(&jana, uri, "0").await.status,
            StatusCode::NOT_FOUND,
            "{uri}"
        );
    }
    let r = e.propfind(&jana, "/Medien/", "infinity").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(r.text().contains("propfind-finite-depth"));
    // A file, and allprop.
    let r = e
        .dav(&jana, "PROPFIND", "/Medien/a.txt", &[("depth", "0")], "")
        .await;
    let t = r.text();
    assert!(t.contains("<D:getetag>") && t.contains("text/plain"), "{t}");
    // Entities are refused, not expanded.
    let evil = r#"<?xml version="1.0"?><!DOCTYPE x [<!ENTITY a "aaaa">]><D:propfind xmlns:D="DAV:"><D:prop><D:a>&a;</D:a></D:prop></D:propfind>"#;
    let r = e
        .dav(&jana, "PROPFIND", "/Medien/", &[("depth", "0")], evil)
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn webdav_put_keeps_the_mtime_and_replaces_in_place() {
    let e = env();
    let jana = e.dav_user("jana", "familie");
    let path = e.root().join("medien/Urlaub Gardasee/notes.txt");
    let oc = [("x-oc-mtime", "1700000000")];
    let r = e
        .dav(
            &jana,
            "PUT",
            "/Medien/Urlaub%20Gardasee/notes.txt",
            &oc,
            "first",
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(r.header("x-oc-mtime"), "accepted");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "first");
    assert_eq!(mtime_of(&path), 1_700_000_000);
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o644
    );
    // Nothing half-written stays behind.
    let uploads = e.root().join("medien/.chestcraft-uploads");
    assert!(std::fs::read_dir(&uploads).map_or(true, |mut d| d.next().is_none()));
    assert!(e.state.db.temps().unwrap().is_empty());

    // Replace: in place, nothing goes to the trash (snapshots keep history).
    let r = e
        .dav(
            &jana,
            "PUT",
            "/Medien/Urlaub%20Gardasee/notes.txt",
            &[],
            "second",
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");
    assert!(e.state.db.trash_items().unwrap().is_empty());
    // Conditional writes.
    let r = e
        .dav(
            &jana,
            "PUT",
            "/Medien/Urlaub%20Gardasee/notes.txt",
            &[("if-none-match", "*")],
            "third",
        )
        .await;
    assert_eq!(r.status, StatusCode::PRECONDITION_FAILED);
    let r = e
        .dav(
            &jana,
            "PUT",
            "/Medien/a.txt",
            &[("if-match", "\"nope\"")],
            "x",
        )
        .await;
    assert_eq!(r.status, StatusCode::PRECONDITION_FAILED);
    assert_eq!(
        std::fs::read_to_string(e.root().join("medien/a.txt")).unwrap(),
        "0123456789"
    );
    // Missing parent, a folder in the way, a reserved name, a dot-name for a non-admin.
    for (uri, code) in [
        ("/Medien/nope/x.txt", StatusCode::CONFLICT),
        ("/Medien/Urlaub%20Gardasee", StatusCode::METHOD_NOT_ALLOWED),
        ("/Medien/.chestcraft-uploads", StatusCode::FORBIDDEN),
        ("/Medien/.hidden", StatusCode::FORBIDDEN),
        ("/Medien/", StatusCode::METHOD_NOT_ALLOWED),
    ] {
        assert_eq!(
            e.dav(&jana, "PUT", uri, &[], "x").await.status,
            code,
            "{uri}"
        );
    }
    assert!(!e.root().join("medien/.hidden").exists());
    let r = e
        .dav(
            &jana,
            "PUT",
            "/Medien/b.txt",
            &[("content-range", "bytes 0-0/1")],
            "x",
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    // A body shorter than announced (a client that went away) leaves nothing behind.
    let r = e
        .dav(
            &jana,
            "PUT",
            "/Medien/short.txt",
            &[("content-length", "100")],
            "x",
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(!e.root().join("medien/short.txt").exists());
    for _ in 0..100 {
        if e.state.db.temps().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(e.state.db.temps().unwrap().is_empty());
    assert_eq!(std::fs::read_dir(&uploads).unwrap().count(), 0);
    e.audited(&["upload"]).await;
}

#[tokio::test]
async fn webdav_rights_decide_what_may_be_replaced() {
    let e = env();
    // familie has "rw" on Eingang: write, but no delete, so only empty files are replaced.
    let jana = e.dav_user("jana", "familie");
    let leser = e.dav_user("lena", "leser");
    std::fs::write(e.root().join("eingang/full.txt"), "data").unwrap();
    std::fs::write(e.root().join("eingang/empty.txt"), "").unwrap();
    let r = e.dav(&jana, "PUT", "/Eingang/full.txt", &[], "new").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(
        std::fs::read_to_string(e.root().join("eingang/full.txt")).unwrap(),
        "data"
    );
    // Windows and Finder create an empty file first, then write it.
    let r = e.dav(&jana, "PUT", "/Eingang/empty.txt", &[], "new").await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = e.dav(&jana, "PUT", "/Eingang/neu.txt", &[], "new").await;
    assert_eq!(r.status, StatusCode::CREATED);
    for (method, uri) in [("DELETE", "/Eingang/neu.txt"), ("MOVE", "/Eingang/neu.txt")] {
        let h = [("destination", "/Eingang/other.txt")];
        assert_eq!(
            e.dav(&jana, method, uri, &h, "").await.status,
            StatusCode::FORBIDDEN
        );
    }
    // Read only: no changes at all, but reading works.
    assert_eq!(
        e.dav(&leser, "PUT", "/Medien/x.txt", &[], "x").await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        e.dav(&leser, "MKCOL", "/Medien/neu", &[], "").await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        e.dav(&leser, "DELETE", "/Medien/a.txt", &[], "")
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    let h = [("destination", "/Medien/b.txt")];
    assert_eq!(
        e.dav(&leser, "COPY", "/Medien/a.txt", &h, "").await.status,
        StatusCode::FORBIDDEN
    );
    // A volume without access can't be a destination either.
    let h = [("destination", "/Backups/a.txt")];
    assert_eq!(
        e.dav(&jana, "COPY", "/Medien/a.txt", &h, "").await.status,
        StatusCode::FORBIDDEN
    );
    assert!(!e.root().join("backups/a.txt").exists());
    assert_eq!(
        e.dav(&jana, "PROPPATCH", "/Medien/a.txt", &[], "")
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn webdav_mkcol_move_copy_and_delete() {
    let e = env();
    let jana = e.dav_user("jana", "familie");
    let m = |p: &str| e.root().join("medien").join(p);
    assert_eq!(
        e.dav(&jana, "MKCOL", "/Medien/Neu", &[], "").await.status,
        StatusCode::CREATED
    );
    assert!(m("Neu").is_dir());
    assert_eq!(
        e.dav(&jana, "MKCOL", "/Medien/Neu", &[], "").await.status,
        StatusCode::METHOD_NOT_ALLOWED
    );
    assert_eq!(
        e.dav(&jana, "MKCOL", "/Medien/a/b", &[], "").await.status,
        StatusCode::CONFLICT
    );
    // MOVE into the new folder, with an absolute URL as rclone sends it.
    let h = [(
        "destination",
        "http://files.local:3923/Medien/Neu/a%20moved.txt",
    )];
    let r = e.dav(&jana, "MOVE", "/Medien/a.txt", &h, "").await;
    assert_eq!(r.status, StatusCode::CREATED);
    assert!(!m("a.txt").exists());
    assert_eq!(
        std::fs::read_to_string(m("Neu/a moved.txt")).unwrap(),
        "0123456789"
    );
    // COPY a folder; Overwrite: F refuses an existing destination.
    let h = [("destination", "/Medien/Kopie/")];
    let r = e
        .dav(&jana, "COPY", "/Medien/Urlaub%20Gardasee/", &h, "")
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
    assert_eq!(std::fs::read(m("Kopie/IMG_2041.jpg")).unwrap(), b"jpegdata");
    assert!(m("Urlaub Gardasee/IMG_2041.jpg").exists());
    let h = [("destination", "/Medien/Kopie"), ("overwrite", "F")];
    let r = e.dav(&jana, "COPY", "/Medien/Neu", &h, "").await;
    assert_eq!(r.status, StatusCode::PRECONDITION_FAILED);
    // MOVE over a file replaces it in place.
    std::fs::write(m("Neu/b.txt"), "b").unwrap();
    let h = [("destination", "/Medien/Neu/b.txt")];
    let r = e
        .dav(&jana, "MOVE", "/Medien/Neu/a%20moved.txt", &h, "")
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(
        std::fs::read_to_string(m("Neu/b.txt")).unwrap(),
        "0123456789"
    );
    // COPY over a folder sends the old one to the trash.
    let h = [("destination", "/Medien/Kopie")];
    let r = e.dav(&jana, "COPY", "/Medien/Neu", &h, "").await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert!(m("Kopie/b.txt").exists() && !m("Kopie/IMG_2041.jpg").exists());
    let trashed = e.state.db.trash_items().unwrap();
    assert_eq!(trashed.len(), 1);
    assert_eq!(
        (trashed[0].path.as_str(), trashed[0].reason.as_str()),
        ("Kopie", "replaced")
    );
    // Refused: onto itself, into a reserved name, the volume root, missing parents.
    for (method, src, dst, code) in [
        ("MOVE", "/Medien/Neu", "/Medien/Neu", StatusCode::FORBIDDEN),
        (
            "MOVE",
            "/Medien/Neu",
            "/Medien/.chestcraft-trash",
            StatusCode::FORBIDDEN,
        ),
        ("MOVE", "/Medien/", "/Medien/x", StatusCode::FORBIDDEN),
        (
            "COPY",
            "/Medien/Neu",
            "/Medien/nope/x",
            StatusCode::CONFLICT,
        ),
        ("MOVE", "/Medien/nope", "/Medien/x", StatusCode::NOT_FOUND),
    ] {
        let h = [("destination", dst)];
        assert_eq!(
            e.dav(&jana, method, src, &h, "").await.status,
            code,
            "{method} {src} {dst}"
        );
    }
    assert_eq!(
        e.dav(&jana, "MOVE", "/Medien/Neu", &[], "").await.status,
        StatusCode::BAD_REQUEST
    );

    // DELETE → trash.
    assert_eq!(
        e.dav(&jana, "DELETE", "/Medien/Neu", &[], "").await.status,
        StatusCode::NO_CONTENT
    );
    assert!(!m("Neu").exists());
    let items = e.state.db.trash_items().unwrap();
    let item = items.iter().find(|i| i.path == "Neu").unwrap();
    assert_eq!(
        (item.reason.as_str(), item.deleted_by.as_str()),
        ("delete", "user:jana")
    );
    assert_eq!(
        e.dav(&jana, "DELETE", "/Medien/", &[], "").await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        e.dav(&jana, "DELETE", "/Medien/Neu", &[], "").await.status,
        StatusCode::NOT_FOUND
    );
    let actions = e.audited(&["mkdir", "move", "copy", "trash"]).await;
    for a in ["mkdir", "move", "copy", "trash"] {
        assert!(actions.iter().any(|x| x == a), "{a}: {actions:?}");
    }
    let ev = e.state.db.recent_audit(50).unwrap();
    assert!(
        ev.iter()
            .any(|a| a.action == "move" && a.detail.contains("webdav laptop rclone"))
    );
}

#[tokio::test]
async fn webdav_proppatch_sets_the_mtime_and_locks_are_advisory() {
    let e = env();
    let jana = e.dav_user("jana", "familie");
    let leser = e.dav_user("lena", "leser");
    let a = e.root().join("medien/a.txt");
    let body = r#"<?xml version="1.0"?><d:propertyupdate xmlns:d="DAV:"><d:set><d:prop><d:lastmodified xmlns="DAV:">1600000000</d:lastmodified></d:prop></d:set></d:propertyupdate>"#;
    let r = e.dav(&jana, "PROPPATCH", "/Medien/a.txt", &[], body).await;
    assert_eq!(r.status, StatusCode::MULTI_STATUS);
    assert!(r.text().contains("HTTP/1.1 200 OK"), "{}", r.text());
    assert_eq!(mtime_of(&a), 1_600_000_000);
    // Without the right to write, or for a property chestcraft doesn't keep: nothing changes.
    let r = e
        .dav(
            &leser,
            "PROPPATCH",
            "/Medien/a.txt",
            &[],
            &body.replace("16", "15"),
        )
        .await;
    assert!(r.text().contains("403"), "{}", r.text());
    let both = r#"<?xml version="1.0"?><D:propertyupdate xmlns:D="DAV:" xmlns:Z="urn:x"><D:set><D:prop><D:lastmodified>1500000000</D:lastmodified><Z:color>red</Z:color></D:prop></D:set></D:propertyupdate>"#;
    let r = e.dav(&jana, "PROPPATCH", "/Medien/a.txt", &[], both).await;
    let t = r.text();
    assert!(
        t.contains("424 Failed Dependency") && t.contains("403 Forbidden"),
        "{t}"
    );
    assert_eq!(mtime_of(&a), 1_600_000_000);

    let lock = r#"<?xml version="1.0"?><D:lockinfo xmlns:D="DAV:"><D:lockscope><D:exclusive/></D:lockscope><D:locktype><D:write/></D:locktype><D:owner>me</D:owner></D:lockinfo>"#;
    let r = e
        .dav(&jana, "LOCK", "/Medien/a.txt", &[("depth", "0")], lock)
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let token = r.header("lock-token").to_string();
    assert!(token.starts_with("<opaquelocktoken:"), "{token}");
    assert!(r.text().contains(&token[1..token.len() - 1]));
    // A refresh keeps the token.
    let r = e
        .dav(
            &jana,
            "LOCK",
            "/Medien/a.txt",
            &[("if", &format!("({token})"))],
            "",
        )
        .await;
    assert_eq!(r.header("lock-token"), token);
    let r = e
        .dav(
            &jana,
            "UNLOCK",
            "/Medien/a.txt",
            &[("lock-token", &token)],
            "",
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn startup_sweeps_temporaries_left_by_a_crash() {
    let e = env();
    let vol = e.state.volumes.get("Medien").unwrap().clone();
    let mut f = crate::fs::transfer::NewFile::create(
        &e.state.db,
        &vol,
        &crate::fs::path::RelPath::root(),
        "big.iso",
        None,
        0,
    )
    .unwrap();
    f.write_all(b"half").unwrap();
    // A crash: the temp entry and its record stay.
    std::mem::forget(f);
    assert_eq!(e.state.db.temps().unwrap().len(), 1);
    let uploads = e.root().join("medien/.chestcraft-uploads");
    assert_eq!(std::fs::read_dir(&uploads).unwrap().count(), 1);
    super::jobs::sweep_leftovers(e.state.clone()).await;
    assert!(e.state.db.temps().unwrap().is_empty());
    assert_eq!(std::fs::read_dir(&uploads).unwrap().count(), 0);
    assert!(!e.root().join("medien/big.iso").exists());
}

// ---------------------------------------------------------------------------------------
// Shares and public uploads

fn attr_after(page: &str, marker: &str, attr: &str) -> String {
    let i = page
        .find(marker)
        .unwrap_or_else(|| panic!("{marker} not in page: {page}"));
    let rest = &page[i..];
    let key = format!("{attr}=\"");
    let j = rest
        .find(&key)
        .unwrap_or_else(|| panic!("no {attr} after {marker}"))
        + key.len();
    let end = rest[j..].find('"').unwrap();
    rest[j..j + end].to_string()
}

fn enc(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// A share visitor: cookie + the CSRF token of the upload API.
fn visitor(cookie: String, csrf: String) -> Login {
    Login { cookie, csrf }
}

impl Env {
    /// Create a share through the form; returns (id, password). `extra` overrides fields.
    async fn create_share(
        &self,
        s: &Login,
        vol: &str,
        path: &str,
        extra: &[(&str, &str)],
    ) -> (String, String) {
        let page = self
            .get_h(
                &format!("/.cc/shares/new?vol={}&path={}", enc(vol), enc(path)),
                &[("cookie", &s.cookie)],
            )
            .await;
        assert_eq!(page.status, StatusCode::OK, "{}", page.text());
        let pp = attr_after(&page.text(), "name=\"passphrase\"", "value");
        let mut pairs: Vec<(&str, &str)> = vec![
            ("vol", vol),
            ("path", path),
            ("passphrase", &pp),
            ("op", "create"),
            ("expiry", "30d"),
            ("mode", "view"),
        ];
        for (k, v) in extra {
            pairs.retain(|(pk, _)| pk != k);
            pairs.push((k, v));
        }
        let r = self.form_post(s, "/.cc/shares/new", &pairs).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        let t = r.text();
        let link = attr_after(&t, "id=\"secret-link\"", "value");
        assert!(link.starts_with("https://files.example.org/s/"), "{link}");
        let password = attr_after(&t, "id=\"secret-password\"", "value");
        // Ready-to-send messages: the creator's language first, then the other one.
        let (en, de) = (t.find("Password: ").unwrap(), t.find("Passwort: ").unwrap());
        assert!(en < de && t.contains("Message (Deutsch)"), "{t}");
        assert!(t.contains("copy.js"), "{t}");
        (link.rsplit('/').next().unwrap().to_string(), password)
    }

    async fn unlock(&self, id: &str, pw: &str, peer: &str, headers: &[(&str, &str)]) -> Res {
        let mut h = vec![FORM];
        h.extend_from_slice(headers);
        self.send_body(
            "POST",
            &format!("/s/{id}/.cc/unlock"),
            peer,
            &h,
            form(&[("password", pw)]),
        )
        .await
    }

    /// Unlock from the LAN; returns the visitor's cookie.
    async fn visit(&self, id: &str, pw: &str) -> String {
        let r = self.unlock(id, pw, "127.0.0.1", &[]).await;
        assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
        assert!(
            r.header("set-cookie").contains(&format!("Path=/s/{id};")),
            "{}",
            r.header("set-cookie")
        );
        cookie_of(&r)
    }

    fn share(&self, id: &str) -> crate::db::Share {
        self.state.db.share(id).unwrap().unwrap()
    }

    /// Share log actions, once `expected` are all written.
    async fn share_log(&self, id: &str, expected: &[&str]) -> Vec<String> {
        let mut actions = Vec::new();
        for _ in 0..100 {
            actions = self
                .state
                .db
                .share_log_of(id, 100)
                .unwrap()
                .into_iter()
                .map(|e| e.action)
                .collect::<Vec<_>>();
            if expected.iter().all(|x| actions.iter().any(|a| a == x)) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        actions
    }
}

#[tokio::test]
async fn folder_share_round_trip() {
    let e = env();
    let admin = e.session("Admin", "admin").await;
    let (id, pw) = e
        .create_share(
            &admin,
            "Medien",
            "Urlaub Gardasee",
            &[("note", "Fotos vom See")],
        )
        .await;
    assert_eq!(pw.split('-').count(), 4, "{pw}");

    // Before the password: nothing about the content.
    let r = e.get(&format!("/s/{id}")).await;
    assert_eq!(r.status, StatusCode::OK);
    let t = r.text();
    assert!(
        t.contains("/.cc/unlock")
            && !t.contains("Urlaub")
            && !t.contains("IMG_2041")
            && !t.contains("Fotos vom See"),
        "{t}"
    );
    assert!(t.contains("lang-switch"), "{t}");
    let r = e.get(&format!("/s/{id}/IMG_2041.jpg")).await;
    assert!(!r.text().contains("jpegdata"));

    let r = e.unlock(&id, "falsch-falsch", "127.0.0.1", &[]).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // Case and separators don't matter for passphrases (phones capitalize).
    let typed = pw.replace('-', " ").to_uppercase();
    let r = e
        .unlock(
            &id,
            &typed,
            "192.0.2.10",
            &[("cf-connecting-ip", "203.0.113.9"), ("cf-ipcountry", "AT")],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    assert_eq!(r.header("location"), format!("/s/{id}/"));
    let cookie = cookie_of(&r);
    let c = [("cookie", cookie.as_str())];

    let t = e.get_h(&format!("/s/{id}/"), &c).await.text();
    assert!(
        t.contains("IMG_2041.jpg") && t.contains("Fotos vom See") && t.contains("Admin"),
        "{t}"
    );
    assert!(!t.contains("data-js=\"upload\""), "view only: {t}");
    let r = e.get_h(&format!("/s/{id}/IMG_2041.jpg"), &c).await;
    assert_eq!(
        (r.status, r.body.as_slice()),
        (StatusCode::OK, b"jpegdata".as_slice())
    );
    let r = e.get_h(&format!("/s/{id}/?zip"), &c).await;
    assert_eq!(zip_names(&r.body), ["IMG_2041.jpg"]);
    let r = e
        .send_body(
            "POST",
            &format!("/s/{id}/"),
            "127.0.0.1",
            &[FORM, ("cookie", &cookie)],
            form(&[("op", "zip"), ("n", "IMG_2041.jpg")]),
        )
        .await;
    assert_eq!(zip_names(&r.body), ["IMG_2041.jpg"]);

    // Nothing outside the shared folder, and no uploads in view mode.
    for bad in ["/../a.txt", "/%2e%2e/a.txt", "/..%2fa.txt", "/.git/"] {
        let r = e.get_h(&format!("/s/{id}{bad}"), &c).await;
        assert!(
            matches!(r.status, StatusCode::NOT_FOUND | StatusCode::BAD_REQUEST),
            "{bad}: {}",
            r.status
        );
    }
    let csrf = e
        .state
        .db
        .guest(&crate::auth::session::hash_id(
            cookie.trim_start_matches("cc_share="),
        ))
        .unwrap()
        .unwrap()
        .csrf;
    let v = visitor(cookie.clone(), csrf);
    let r = e
        .upload_at(&format!("/s/{id}/.cc"), &v, "", "", "x.txt", b"x", "rename")
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());

    // The owner sees opens, downloads and the log with the visitor's country.
    let log = e
        .share_log(&id, &["unlock", "download", "zip", "unlock_failed"])
        .await;
    assert!(log.contains(&"unlock_failed".to_string()), "{log:?}");
    let s = e.share(&id);
    assert_eq!((s.opens, s.downloads), (1, 3));
    let t = e
        .get_h(&format!("/.cc/shares/{id}"), &[("cookie", &admin.cookie)])
        .await
        .text();
    assert!(t.contains("203.0.113.9") && t.contains(">AT<"), "{t}");
    let t = e
        .get_h("/.cc/shares", &[("cookie", &admin.cookie)])
        .await
        .text();
    assert!(t.contains("Medien/Urlaub Gardasee"), "{t}");

    // Revoked: the link says so, the cookie is dead.
    let r = e
        .form_post(
            &admin,
            &format!("/.cc/shares/{id}"),
            &[("op", "revoke"), ("confirm", "1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let r = e.get_h(&format!("/s/{id}/"), &c).await;
    assert_eq!(r.status, StatusCode::GONE);
    assert!(
        e.state
            .db
            .guest(&crate::auth::session::hash_id(
                cookie.trim_start_matches("cc_share=")
            ))
            .unwrap()
            .is_none()
    );
    let r = e
        .form_post(&admin, &format!("/.cc/shares/{id}"), &[("op", "password")])
        .await;
    assert_eq!(
        r.status,
        StatusCode::CONFLICT,
        "revoked shares can't change"
    );
}

#[tokio::test]
async fn file_share_expiry_and_unknown_links() {
    let e = env();
    let admin = e.session("Admin", "admin").await;
    let (id, pw) = e
        .create_share(
            &admin,
            "Medien",
            "a.txt",
            &[("password_kind", "own"), ("own_password", "Mein Passwort")],
        )
        .await;
    assert_eq!(pw, "Mein Passwort");
    assert_eq!(
        e.unlock(&id, "mein passwort", "127.0.0.1", &[])
            .await
            .status,
        StatusCode::UNAUTHORIZED,
        "own passwords are exact"
    );
    let cookie = e.visit(&id, "Mein Passwort").await;
    let c = [("cookie", cookie.as_str())];
    let t = e.get_h(&format!("/s/{id}"), &c).await.text();
    assert!(
        t.contains("a.txt") && t.contains(&format!("/s/{id}/a.txt?dl")),
        "{t}"
    );
    let r = e.get_h(&format!("/s/{id}/a.txt"), &c).await;
    assert_eq!(r.body, b"0123456789");
    assert_eq!(
        e.get_h(&format!("/s/{id}/b.txt"), &c).await.status,
        StatusCode::NOT_FOUND
    );
    // The cookie belongs to this share only.
    let (other, _) = e.create_share(&admin, "Medien", "page.html", &[]).await;
    let t = e.get_h(&format!("/s/{other}"), &c).await.text();
    assert!(t.contains("/.cc/unlock"), "{t}");

    e.state
        .db
        .update_share(&id, true, |s| s.expires = Some(1))
        .unwrap();
    let r = e.get_h(&format!("/s/{id}/a.txt"), &c).await;
    assert_eq!(r.status, StatusCode::GONE);
    assert!(r.text().contains("expired"), "{}", r.text());
    assert_eq!(e.get("/s/123456789abc").await.status, StatusCode::NOT_FOUND);
    assert_eq!(e.get("/s/nope").await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn upload_only_shares_never_overwrite_and_show_only_own_uploads() {
    let e = env();
    let jana = e.session("jana", "teiler").await;
    let (id, pw) = e
        .create_share(
            &jana,
            "Medien",
            "Urlaub Gardasee",
            &[("mode", "upload"), ("max_mib", "1")],
        )
        .await;
    let cookie = e.visit(&id, &pw).await;
    let c = [("cookie", cookie.as_str())];
    let page = e.get_h(&format!("/s/{id}"), &c).await.text();
    assert!(!page.contains("IMG_2041"), "{page}");
    assert!(
        page.contains(&format!("data-api=\"/s/{id}/.cc\"")),
        "{page}"
    );
    let csrf = attr_after(&page, "data-js=\"upload\"", "data-csrf");
    let v = visitor(cookie.clone(), csrf);
    let base = format!("/s/{id}/.cc");

    // A taken name is kept as a copy, even when "replace" is asked for.
    let r = e
        .upload_at(&base, &v, "", "", "IMG_2041.jpg", b"new photo", "replace")
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(json_of(&r)["path"], "IMG_2041 (1).jpg");
    let dir = e.root().join("medien/Urlaub Gardasee");
    assert_eq!(
        std::fs::read(dir.join("IMG_2041.jpg")).unwrap(),
        b"jpegdata"
    );
    assert_eq!(
        std::fs::read(dir.join("IMG_2041 (1).jpg")).unwrap(),
        b"new photo"
    );
    let r = e
        .upload_at(&base, &v, "", "", "Scan/sub/b.pdf", b"pdf", "ask")
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(dir.join("Scan/sub/b.pdf").is_file());

    // Above the size limit.
    let big = vec![0u8; MIB + 1];
    let r = e.upload_at(&base, &v, "", "", "big.bin", &big, "ask").await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE, "{}", r.text());
    assert_eq!(json_of(&r)["error"], "too_large");

    // The page lists this visitor's uploads, nothing else; downloads don't exist.
    let page = e.get_h(&format!("/s/{id}"), &c).await.text();
    assert!(
        page.contains("IMG_2041 (1).jpg") && page.contains("Scan/sub/b.pdf"),
        "{page}"
    );
    assert!(!page.contains("IMG_2041.jpg<"), "{page}");
    assert_eq!(
        e.get_h(&format!("/s/{id}/IMG_2041.jpg"), &c).await.status,
        StatusCode::NOT_FOUND
    );
    // A second visitor doesn't see the first one's uploads.
    let other = e.visit(&id, &pw).await;
    let page = e
        .get_h(&format!("/s/{id}"), &[("cookie", &other)])
        .await
        .text();
    assert!(!page.contains("IMG_2041 (1).jpg"), "{page}");

    // No CSRF token, no upload.
    let r = e
        .send_body(
            "POST",
            &format!("{base}/upload"),
            "127.0.0.1",
            &[JSON, ("cookie", &cookie)],
            serde_json::json!({"dir": "", "name": "y.txt", "size": 1}).to_string(),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(e.share(&id).uploads, 2);
    let log = e.share_log(&id, &["upload"]).await;
    assert_eq!(log.iter().filter(|a| *a == "upload").count(), 2, "{log:?}");
}

#[tokio::test]
async fn view_and_upload_share_uploads_into_subfolders() {
    let e = env();
    let jana = e.session("jana", "teiler").await;
    std::fs::create_dir(e.root().join("medien/Urlaub Gardasee/Tag 2")).unwrap();
    let (id, pw) = e
        .create_share(
            &jana,
            "Medien",
            "Urlaub Gardasee",
            &[("mode", "view_upload")],
        )
        .await;
    let cookie = e.visit(&id, &pw).await;
    let page = e
        .get_h(&format!("/s/{id}/Tag%202/"), &[("cookie", &cookie)])
        .await
        .text();
    assert!(page.contains("data-dir=\"Tag 2\""), "{page}");
    let csrf = attr_after(&page, "data-js=\"upload\"", "data-csrf");
    let v = visitor(cookie, csrf);
    let r = e
        .upload_at(
            &format!("/s/{id}/.cc"),
            &v,
            "Medien",
            "Tag 2",
            "c.jpg",
            b"c",
            "ask",
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(
        e.root()
            .join("medien/Urlaub Gardasee/Tag 2/c.jpg")
            .is_file()
    );
    // `dir` is relative to the share: no way out.
    let r = e
        .upload_at(
            &format!("/s/{id}/.cc"),
            &v,
            "Medien",
            "../x",
            "d.jpg",
            b"d",
            "ask",
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
}

#[tokio::test]
async fn shares_never_exceed_the_creator_and_pause_with_them() {
    let e = env();
    let leser = e.session("mika", "leser").await;
    let r = e
        .get_h(
            "/.cc/shares/new?vol=Medien&path=a.txt",
            &[("cookie", &leser.cookie)],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let t = e
        .get_h("/Medien/", &[("cookie", &leser.cookie)])
        .await
        .text();
    assert!(!t.contains("/.cc/shares/new"), "{t}");

    // Home-only volumes can't be shared, with the reason in the listing.
    let admin = e.session("Admin", "admin").await;
    let t = e
        .get_h("/Backups/", &[("cookie", &admin.cookie)])
        .await
        .text();
    assert!(
        t.contains("only reachable at home") && !t.contains("/.cc/shares/new"),
        "{t}"
    );
    let r = e
        .get_h(
            "/.cc/shares/new?vol=Backups&path=dump.bin",
            &[("cookie", &admin.cookie)],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);

    // A sharing role gets links and badges.
    let jana = e.session("jana", "teiler").await;
    let t = e
        .get_h("/Medien/", &[("cookie", &jana.cookie)])
        .await
        .text();
    assert!(
        t.contains("/.cc/shares/new?vol=Medien&amp;path=a.txt")
            || t.contains("/.cc/shares/new?vol=Medien&path=a.txt"),
        "{t}"
    );
    let r = e
        .get_h(
            "/.cc/shares/new?vol=Medien&path=a.txt",
            &[("cookie", &jana.cookie)],
        )
        .await
        .text();
    assert!(
        !r.contains("value=\"upload\""),
        "files can't take uploads: {r}"
    );
    let (id, pw) = e
        .create_share(&jana, "Medien", "Urlaub Gardasee", &[])
        .await;
    let t = e
        .get_h("/Medien/", &[("cookie", &jana.cookie)])
        .await
        .text();
    assert!(t.contains("data-state=\"shared\""), "{t}");
    let cookie = e.visit(&id, &pw).await;
    let c = [("cookie", cookie.as_str())];

    // Creator disabled → paused; enabled again → works.
    let mut u = e.state.db.user("jana").unwrap().unwrap();
    u.disabled = true;
    e.state.db.put_user(&u).unwrap();
    assert_eq!(
        e.get_h(&format!("/s/{id}/"), &c).await.status,
        StatusCode::FORBIDDEN
    );
    u.disabled = false;
    e.state.db.put_user(&u).unwrap();
    assert_eq!(
        e.get_h(&format!("/s/{id}/"), &c).await.status,
        StatusCode::OK
    );
    // Role lost the right to share → paused.
    u.role = "leser".into();
    e.state.db.put_user(&u).unwrap();
    assert_eq!(
        e.get_h(&format!("/s/{id}/"), &c).await.status,
        StatusCode::FORBIDDEN
    );
    u.role = "teiler".into();
    e.state.db.put_user(&u).unwrap();

    // Target in the trash → paused; restored → works again.
    let r = e
        .form_post(
            &jana,
            "/Medien/",
            &[("op", "delete"), ("n", "Urlaub Gardasee")],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    let r = e.get_h(&format!("/s/{id}/"), &c).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(r.text().contains("paused"), "{}", r.text());
    let item = e.state.db.trash_items().unwrap().pop().unwrap();
    let r = e
        .form_post(&jana, "/.cc/trash/restore", &[("id", &item.id)])
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(
        e.get_h(&format!("/s/{id}/"), &c).await.status,
        StatusCode::OK
    );

    // Moved through WebDAV → the share follows; moved behind chestcraft's back → missing.
    let auth = e.dav_user("admin2", "admin");
    let h = [("destination", "/Medien/Urlaub%202025")];
    let r = e
        .dav(&auth, "MOVE", "/Medien/Urlaub%20Gardasee", &h, "")
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(e.share(&id).path, "Urlaub 2025");
    let t = e.get_h(&format!("/s/{id}/"), &c).await.text();
    assert!(t.contains("IMG_2041.jpg"), "{t}");
    std::fs::rename(
        e.root().join("medien/Urlaub 2025"),
        e.root().join("medien/weg"),
    )
    .unwrap();
    let r = e.get_h(&format!("/s/{id}/"), &c).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let t = e
        .get_h("/.cc/shares", &[("cookie", &jana.cookie)])
        .await
        .text();
    assert!(t.contains("data-state=\"missing\""), "{t}");
}

#[tokio::test]
async fn wrong_share_passwords_back_off_per_address() {
    let e = env();
    let admin = e.session("Admin", "admin").await;
    let (id, pw) = e.create_share(&admin, "Medien", "a.txt", &[]).await;
    let remote = |ip: &'static str| [("cf-connecting-ip", ip)];
    for _ in 0..crate::auth::backoff::FREE_TRIES {
        let r = e
            .unlock(&id, "nein-nein-nein", "192.0.2.10", &remote("203.0.113.5"))
            .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    }
    let r = e
        .unlock(&id, &pw, "192.0.2.10", &remote("203.0.113.5"))
        .await;
    assert_eq!(
        r.status,
        StatusCode::TOO_MANY_REQUESTS,
        "even the right password waits"
    );
    assert!(r.text().contains("min"), "{}", r.text());
    let r = e
        .unlock(&id, &pw, "192.0.2.10", &remote("203.0.113.6"))
        .await;
    assert_eq!(
        r.status,
        StatusCode::SEE_OTHER,
        "another address isn't affected"
    );
}

#[tokio::test]
async fn a_new_share_password_ends_visitor_sessions() {
    let e = env();
    let admin = e.session("Admin", "admin").await;
    let (id, pw) = e
        .create_share(&admin, "Medien", "Urlaub Gardasee", &[])
        .await;
    let cookie = e.visit(&id, &pw).await;
    let r = e
        .form_post(&admin, &format!("/.cc/shares/{id}"), &[("op", "password")])
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let new_pw = attr_after(&r.text(), "id=\"secret-password\"", "value");
    assert_ne!(new_pw, pw);
    let t = e
        .get_h(&format!("/s/{id}/"), &[("cookie", &cookie)])
        .await
        .text();
    assert!(t.contains("/.cc/unlock"), "{t}");
    assert_eq!(
        e.unlock(&id, &pw, "127.0.0.1", &[]).await.status,
        StatusCode::UNAUTHORIZED
    );
    e.visit(&id, &new_pw).await;

    // Edit: mode and expiry; a file can't become an upload share; others can't edit.
    let r = e
        .form_post(
            &admin,
            &format!("/.cc/shares/{id}"),
            &[
                ("op", "edit"),
                ("mode", "view_upload"),
                ("expiry", "1d"),
                ("note", "neu"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let s = e.share(&id);
    assert_eq!((s.mode.as_str(), s.note.as_str()), ("view_upload", "neu"));
    assert!(s.expires.unwrap() - crate::web::now() <= 86_400);
    let jana = e.session("jana", "teiler").await;
    let r = e
        .get_h(&format!("/.cc/shares/{id}"), &[("cookie", &jana.cookie)])
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = e
        .form_post(
            &jana,
            &format!("/.cc/shares/{id}"),
            &[("op", "revoke"), ("confirm", "1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert!(e.share(&id).revoked.is_none());
}

#[tokio::test]
async fn public_uploads_open_with_a_password_and_never_overwrite() {
    let e = env();
    // Closed: no upload offer, and the upload API refuses anonymous visitors.
    let t = e.get("/Share/").await.text();
    assert!(!t.contains("/.cc/public-write"), "{t}");
    let admin = e.session("Admin", "admin").await;
    let jana = e.session("jana", "teiler").await;
    assert_eq!(
        e.get_h("/.cc/admin/volumes", &[("cookie", &jana.cookie)])
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let r = e
        .form_post(
            &admin,
            "/.cc/admin/volumes",
            &[("op", "open"), ("vol", "Medien")],
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let r = e
        .form_post(
            &admin,
            "/.cc/admin/volumes",
            &[
                ("op", "open"),
                ("vol", "Share"),
                ("expiry", "7d"),
                ("own_password", "Briefkasten 7"),
                ("max_mib", "1"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        attr_after(&r.text(), "id=\"secret-password\"", "value"),
        "Briefkasten 7"
    );
    let t = e
        .get_h("/.cc/admin/volumes", &[("cookie", &admin.cookie)])
        .await
        .text();
    assert!(t.contains("data-state=\"open\""), "{t}");

    let t = e.get("/Share/").await.text();
    assert!(t.contains("/.cc/public-write?vol=Share"), "{t}");
    let unlock = |pw: &str| form(&[("vol", "Share"), ("password", pw), ("next", "/Share/")]);
    let r = e
        .send_body(
            "POST",
            "/.cc/public-write",
            "127.0.0.1",
            &[FORM],
            unlock("falsch"),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = e
        .send_body(
            "POST",
            "/.cc/public-write",
            "127.0.0.1",
            &[FORM],
            unlock("Briefkasten 7"),
        )
        .await;
    assert_eq!(
        (r.status, r.header("location")),
        (StatusCode::SEE_OTHER, "/Share/")
    );
    assert!(
        r.header("set-cookie").contains("Path=/;"),
        "{}",
        r.header("set-cookie")
    );
    let cookie = cookie_of(&r);
    let page = e.get_h("/Share/", &[("cookie", &cookie)]).await.text();
    let csrf = attr_after(&page, "data-js=\"upload\"", "data-csrf");
    assert!(page.contains("name=\"op\" value=\"mkdir\""), "{page}");
    let v = visitor(cookie.clone(), csrf.clone());

    let r = e
        .upload(&v, "Share", "", "hello.txt", b"replaced?", "replace")
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(json_of(&r)["path"], "hello (1).txt");
    assert_eq!(
        std::fs::read(e.root().join("share/hello.txt")).unwrap(),
        b"hello"
    );
    let r = e
        .upload(&v, "Share", "", "big.bin", &vec![0u8; MIB + 1], "ask")
        .await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
    let r = e.upload(&v, "Medien", "", "x.txt", b"x", "ask").await;
    assert!(
        matches!(r.status, StatusCode::NOT_FOUND | StatusCode::FORBIDDEN),
        "{}",
        r.status
    );
    let r = e
        .form_post(&v, "/Share/", &[("op", "mkdir"), ("name", "Abgabe")])
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    assert!(e.root().join("share/Abgabe").is_dir());
    let r = e
        .form_post(&v, "/Share/", &[("op", "delete"), ("n", "hello.txt")])
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(e.root().join("share/hello.txt").is_file());
    assert!(
        e.audited(&["public_upload", "public_unlock"])
            .await
            .contains(&"public_upload".to_string())
    );

    // Opening again sets a new password and ends the sessions; closing ends uploads.
    let r = e
        .form_post(
            &admin,
            "/.cc/admin/volumes",
            &[("op", "open"), ("vol", "Share"), ("expiry", "1d")],
        )
        .await;
    let generated = attr_after(&r.text(), "id=\"secret-password\"", "value");
    assert_eq!(generated.split('-').count(), 4);
    let r = e.upload(&v, "Share", "", "late.txt", b"x", "ask").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text());
    let r = e
        .send_body(
            "POST",
            "/.cc/public-write",
            "127.0.0.1",
            &[FORM],
            unlock(&generated),
        )
        .await;
    let v2 = visitor(cookie_of(&r), String::new());
    let page = e.get_h("/Share/", &[("cookie", &v2.cookie)]).await.text();
    let v2 = visitor(
        v2.cookie,
        attr_after(&page, "data-js=\"upload\"", "data-csrf"),
    );
    assert_eq!(
        e.upload(&v2, "Share", "", "ok.txt", b"ok", "ask")
            .await
            .status,
        StatusCode::OK
    );
    let r = e
        .form_post(
            &admin,
            "/.cc/admin/volumes",
            &[("op", "close"), ("vol", "Share")],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(
        e.upload(&v2, "Share", "", "closed.txt", b"x", "ask")
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    assert!(!e.get("/Share/").await.text().contains("/.cc/public-write"));
    let _ = csrf;
}

#[tokio::test]
async fn public_uploads_close_by_themselves() {
    let e = env();
    e.state
        .db
        .put_public_write(&crate::db::PublicWrite {
            volume: "Share".into(),
            password_hash: crate::auth::password::hash("Briefkasten 7"),
            passphrase: false,
            opened: 0,
            until: Some(1),
            max_file: None,
            notify: false,
            by: "admin".into(),
        })
        .unwrap();
    assert!(!e.get("/Share/").await.text().contains("/.cc/public-write"));
    let r = e
        .send_body(
            "POST",
            "/.cc/public-write",
            "127.0.0.1",
            &[FORM],
            form(&[("vol", "Share"), ("password", "Briefkasten 7")]),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn language_switch_sets_a_cookie_and_stays_on_the_site() {
    let e = env();
    let r = e.get("/.cc/lang?set=de&next=%2Fs%2Fabc").await;
    assert_eq!(
        (r.status, r.header("location")),
        (StatusCode::SEE_OTHER, "/s/abc")
    );
    assert!(r.header("set-cookie").starts_with("cc_lang=de;"));
    let r = e
        .get("/.cc/lang?set=xx&next=https%3A%2F%2Fevil.example")
        .await;
    assert_eq!(r.header("location"), "/");
    assert!(r.header("set-cookie").is_empty());
}
