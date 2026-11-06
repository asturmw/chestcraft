//! MCP over HTTP against the full MCP router with temp-dir volumes: the handshake of both
//! protocol eras, the gate (network, Host, Origin, tokens, limits), the permission matrix
//! and the never-overwrite rules.

use super::*;
use crate::config::Config;
use crate::fs::path::RelPath;
use crate::health::alerts::LogEntry;
use crate::health::{Probe, Snapshot, zpool};
use crate::i18n::I18n;
use crate::web::AppState;
use axum::body::Body;
use axum::http::Request as HttpRequest;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde_json::{Value, json};
use tower::ServiceExt;

const HOST: &str = "mcp.test:3925";
const MODERN: &str = "2026-07-28";

struct Env {
    _tmp: tempfile::TempDir,
    root: std::path::PathBuf,
    st: Shared,
    router: Router,
    reader: String,
    writer: String,
    ops: String,
}

struct Res {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Res {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| {
            panic!(
                "{e}: {} {}",
                self.status,
                String::from_utf8_lossy(&self.body)
            )
        })
    }
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

fn tiny_png() -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(2, 2, image::Rgba([200, 30, 30, 255]));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

fn env_with(mcp_extra: &str) -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let r = tmp.path().to_path_buf();
    for d in [
        "docs/sub",
        "docs/.hidden",
        "docs/.hist",
        "photos",
        "local",
        "archive",
        "secret",
    ] {
        std::fs::create_dir_all(r.join(d)).unwrap();
    }
    std::fs::write(r.join("docs/readme.txt"), "hello world\n").unwrap();
    std::fs::write(r.join("docs/sub/a.txt"), "a").unwrap();
    std::fs::write(r.join("docs/.dotfile"), "dot").unwrap();
    std::fs::write(r.join("docs/.hist/x"), "internal").unwrap();
    std::fs::write(r.join("docs/bin.dat"), [0u8, 1, 2, 255, 0, 7]).unwrap();
    std::fs::write(r.join("photos/pic.png"), tiny_png()).unwrap();
    std::fs::write(r.join("archive/old.txt"), "old").unwrap();
    std::fs::write(r.join("secret/s.txt"), "secret").unwrap();
    std::fs::write(r.join("local/l.txt"), "local").unwrap();
    let cfg = Config::from_toml_str(&format!(
        r#"
        [server]
        public_url = "https://files.example.org"
        trusted_proxies = ["192.0.2.10"]
        remote_proxies = ["192.0.2.10"]
        [paths]
        state_dir = "{0}/state"
        cache_dir = "{0}/cache"
        backup_dir = "{0}/backup"
        [volumes.Docs]
        path = "{0}/docs"
        [volumes.Photos]
        path = "{0}/photos"
        [volumes.Local]
        path = "{0}/local"
        remote = false
        [volumes.Archive]
        path = "{0}/archive"
        read_only = true
        [volumes.Secret]
        path = "{0}/secret"
        [roles.reader]
        Docs = "r"
        Photos = "r"
        Archive = "r"
        [roles.writer]
        Docs = "rwmds"
        Photos = "rw"
        Local = "rwmds"
        Archive = "rwmds"
        [roles.ops]
        Docs = "r"
        [mcp]
        listen = "127.0.0.1:3925"
        hosts = ["{HOST}", "localhost"]
        health_roles = ["ops"]
        max_read_mib = 1
        max_write_mib = 1
        {mcp_extra}
        "#,
        r.display()
    ))
    .unwrap();
    let db = crate::web::open_db(&cfg).unwrap();
    let st = Arc::new(AppState::new(cfg, I18n::builtin(), db, &Default::default()));
    let now = now();
    let mut tokens = Vec::new();
    for (name, role) in [("reader", "reader"), ("writer", "writer"), ("ops", "ops")] {
        agent::create(&st.db, &st.cfg, name, role, now).unwrap();
        tokens.push(
            agent::create_token(&st.db, name, "test", None, "cli", now)
                .unwrap()
                .0,
        );
    }
    let router = super::router(st.clone(), CancellationToken::new());
    Env {
        _tmp: tmp,
        root: r,
        st,
        router,
        ops: tokens.pop().unwrap(),
        writer: tokens.pop().unwrap(),
        reader: tokens.pop().unwrap(),
    }
}

fn env() -> Env {
    env_with("")
}

fn meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": MODERN,
        "io.modelcontextprotocol/clientInfo": { "name": "test", "version": "1" },
        "io.modelcontextprotocol/clientCapabilities": {}
    })
}

impl Env {
    async fn raw(&self, method: &str, peer: &str, headers: &[(&str, &str)], body: String) -> Res {
        let mut b = HttpRequest::builder().method(method).uri(PATH);
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        let mut req = b.body(Body::from(body)).unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::new(peer.parse().unwrap(), 40000)));
        let res = self.router.clone().oneshot(req).await.unwrap();
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

    /// A 2026-07-28 request with every header the revision wants.
    async fn modern(
        &self,
        token: &str,
        method: &str,
        mut params: Value,
        extra: &[(&str, &str)],
    ) -> Res {
        params["_meta"] = meta();
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string);
        let body = json!({ "jsonrpc": "2.0", "id": 7, "method": method, "params": params });
        let auth = format!("Bearer {token}");
        let mut h: Vec<(&str, &str)> = vec![
            ("host", HOST),
            ("content-type", "application/json"),
            ("accept", "application/json, text/event-stream"),
            ("authorization", &auth),
            ("mcp-protocol-version", MODERN),
            ("mcp-method", method),
        ];
        if let Some(n) = &name {
            h.push(("mcp-name", n));
        }
        for (k, v) in extra {
            h.retain(|(name, _)| name != k);
            h.push((k, v));
        }
        self.raw("POST", "127.0.0.1", &h, body.to_string()).await
    }

    /// `tools/call` → the result object.
    async fn call(&self, token: &str, tool: &str, args: Value) -> Value {
        let r = self
            .modern(
                token,
                "tools/call",
                json!({ "name": tool, "arguments": args }),
                &[],
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{tool}: {}", r.text());
        let v = r.json();
        assert!(v.get("error").is_none(), "{tool}: {v}");
        v["result"].clone()
    }

    /// A successful call's structured result.
    async fn ok(&self, token: &str, tool: &str, args: Value) -> Value {
        let r = self.call(token, tool, args).await;
        assert_eq!(r["isError"], json!(false), "{tool}: {r}");
        r["structuredContent"].clone()
    }

    /// A refused call's error code.
    async fn err(&self, token: &str, tool: &str, args: Value) -> String {
        let r = self.call(token, tool, args).await;
        assert_eq!(r["isError"], json!(true), "{tool} should fail: {r}");
        r["structuredContent"]["error"]
            .as_str()
            .unwrap()
            .to_string()
    }

    async fn tool_names(&self, token: &str) -> Vec<String> {
        let r = self.modern(token, "tools/list", json!({}), &[]).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        r.json()["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    }

    fn read(&self, rel: &str) -> Option<Vec<u8>> {
        std::fs::read(self.root.join(rel)).ok()
    }
}

// ---------------------------------------------------------------------------------------
// Handshakes

#[tokio::test]
async fn modern_discover_and_list() {
    let e = env();
    let r = e.modern(&e.reader, "server/discover", json!({}), &[]).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(
        r.headers["content-type"]
            .to_str()
            .unwrap()
            .starts_with("application/json")
    );
    assert!(r.headers.get("mcp-session-id").is_none());
    let v = r.json();
    let res = &v["result"];
    assert!(
        res["supportedVersions"]
            .as_array()
            .unwrap()
            .contains(&json!(MODERN)),
        "{v}"
    );
    assert!(res["capabilities"]["tools"].is_object(), "{v}");
    assert!(
        res["instructions"]
            .as_str()
            .unwrap()
            .contains("ever overwrites a file")
    );

    let names = e.tool_names(&e.writer).await;
    for t in [
        "volumes",
        "list",
        "read_file",
        "write_file",
        "delete",
        "share_create",
    ] {
        assert!(names.contains(&t.to_string()), "{t}: {names:?}");
    }
    // Annotations and the rules in the descriptions (Hermes ignores server instructions).
    let r = e
        .modern(&e.writer, "tools/list", json!({}), &[])
        .await
        .json();
    let tools = r["result"]["tools"].as_array().unwrap();
    let get = |n: &str| tools.iter().find(|t| t["name"] == n).unwrap().clone();
    assert_eq!(get("list")["annotations"]["readOnlyHint"], json!(true));
    assert_eq!(get("delete")["annotations"]["destructiveHint"], json!(true));
    assert_eq!(
        get("write_file")["annotations"]["destructiveHint"],
        json!(false)
    );
    assert!(
        get("write_file")["description"]
            .as_str()
            .unwrap()
            .contains("Never overwrites")
    );
    assert!(
        get("delete")["description"]
            .as_str()
            .unwrap()
            .contains("trash")
    );
    assert_eq!(get("list")["inputSchema"]["type"], json!("object"));
}

#[tokio::test]
async fn legacy_initialize_then_tools() {
    let e = env();
    let auth = format!("Bearer {}", e.reader);
    let base = [
        ("host", HOST),
        ("content-type", "application/json"),
        ("accept", "application/json, text/event-stream"),
        ("authorization", auth.as_str()),
    ];
    let init = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-11-25", "capabilities": {},
        "clientInfo": { "name": "hermes", "version": "1" } } });
    let r = e.raw("POST", "127.0.0.1", &base, init.to_string()).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let v = r.json();
    assert_eq!(v["result"]["protocolVersion"], json!("2025-11-25"), "{v}");
    assert_eq!(v["result"]["serverInfo"]["name"], json!("chestcraft"));
    assert!(r.headers.get("mcp-session-id").is_none(), "stateless");

    let mut h = base.to_vec();
    h.push(("mcp-protocol-version", "2025-11-25"));
    let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    let r = e.raw("POST", "127.0.0.1", &h, note.to_string()).await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());

    let list = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" });
    let r = e.raw("POST", "127.0.0.1", &h, list.to_string()).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.json()["result"]["tools"].as_array().unwrap().len() >= 5);

    let call = json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                       "params": { "name": "list", "arguments": { "path": "/Docs" } } });
    let r = e.raw("POST", "127.0.0.1", &h, call.to_string()).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let v = r.json();
    assert_eq!(v["result"]["isError"], json!(false), "{v}");
}

// ---------------------------------------------------------------------------------------
// The gate

#[tokio::test]
async fn refused_from_the_internet_wrong_host_or_origin() {
    let e = env();
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }).to_string();
    let auth = format!("Bearer {}", e.reader);
    let good = |extra: &[(&'static str, &'static str)]| {
        let mut h: Vec<(&str, String)> = vec![
            ("host", HOST.into()),
            ("content-type", "application/json".into()),
            ("accept", "application/json, text/event-stream".into()),
            ("authorization", auth.clone()),
        ];
        for (k, v) in extra {
            h.retain(|(name, _)| name != k);
            h.push((k, v.to_string()));
        }
        h
    };
    let send = |peer: &'static str, h: Vec<(&'static str, String)>| {
        let body = body.clone();
        let e = &e;
        async move {
            let refs: Vec<(&str, &str)> = h.iter().map(|(k, v)| (*k, v.as_str())).collect();
            e.raw("POST", peer, &refs, body).await
        }
    };
    // Through the tunnel, with Cloudflare headers, or from outside the local networks.
    let r = send("192.0.2.10", good(&[("cf-connecting-ip", "198.51.100.7")])).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let r = send("127.0.0.1", good(&[("cf-ray", "abc")])).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = send("198.51.100.7", good(&[])).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // DNS rebinding: a foreign Host or Origin.
    let r = send("127.0.0.1", good(&[("host", "evil.example:3925")])).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = send("127.0.0.1", good(&[("host", "mcp.test:9999")])).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "port must match");
    let r = send("127.0.0.1", good(&[("origin", "https://evil.example")])).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // A host entry without a port matches any port; Tailscale counts as local.
    let r = send("100.64.0.9", good(&[("host", "localhost:1234")])).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // Other paths don't exist.
    let mut req = HttpRequest::builder()
        .uri("/other")
        .header("host", HOST)
        .body(Body::empty())
        .unwrap();
    req.extensions_mut().insert(ConnectInfo(SocketAddr::new(
        "127.0.0.1".parse().unwrap(),
        1,
    )));
    let res = e.router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn tokens_missing_wrong_revoked_expired_or_disabled() {
    let e = env();
    let list = |token: String| {
        let e = &e;
        async move { e.modern(&token, "tools/list", json!({}), &[]).await }
    };
    let r = e
        .raw(
            "POST",
            "127.0.0.1",
            &[("host", HOST), ("content-type", "application/json")],
            "{}".into(),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert!(
        r.headers["www-authenticate"]
            .to_str()
            .unwrap()
            .starts_with("Bearer")
    );

    // A status API token or a made-up one is no agent token.
    let status_token = crate::auth::apitoken::create(&e.st.db, "w", "cli", now())
        .unwrap()
        .0;
    assert_eq!(list(status_token).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        list(agent::generate()).await.status,
        StatusCode::UNAUTHORIZED
    );

    // Revoked.
    let (t, rec) = agent::create_token(&e.st.db, "reader", "laptop", None, "cli", now()).unwrap();
    assert_eq!(list(t.clone()).await.status, StatusCode::OK);
    agent::revoke(&e.st.db, "reader", &rec.id).unwrap();
    assert_eq!(list(t).await.status, StatusCode::UNAUTHORIZED);
    // Expired.
    let (t, _) =
        agent::create_token(&e.st.db, "reader", "short", Some(now() + 1), "cli", now()).unwrap();
    let hash = agent::hash(&t);
    let mut rec = e.st.db.agent_token(&hash).unwrap().unwrap();
    rec.expires = Some(now() - 1);
    e.st.db.put_agent_token(&hash, &rec, true).unwrap();
    assert_eq!(list(t).await.status, StatusCode::UNAUTHORIZED);
    // Disabled agent: its tokens stop.
    e.st.db
        .update_user("reader", |u| u.disabled = true)
        .unwrap();
    assert_eq!(
        list(e.reader.clone()).await.status,
        StatusCode::UNAUTHORIZED
    );
    e.st.db
        .update_user("reader", |u| u.disabled = false)
        .unwrap();
    assert_eq!(list(e.reader.clone()).await.status, StatusCode::OK);

    // Five wrong tokens in a row → the address waits, even with a good token.
    for _ in 0..5 {
        assert_eq!(
            list(agent::generate()).await.status,
            StatusCode::UNAUTHORIZED
        );
    }
    let r = list(e.reader.clone()).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS, "{}", r.text());
    assert!(r.headers.contains_key("retry-after"));
    // Audited.
    let failed =
        e.st.db
            .recent_audit(100)
            .unwrap()
            .iter()
            .filter(|a| a.action == "mcp_token_failed")
            .count();
    assert!(failed >= 5, "{failed}");
}

#[tokio::test]
async fn calls_per_minute_per_agent() {
    let e = env_with("calls_per_minute = 2");
    for _ in 0..2 {
        assert_eq!(
            e.modern(&e.reader, "tools/list", json!({}), &[])
                .await
                .status,
            StatusCode::OK
        );
    }
    let r = e.modern(&e.reader, "tools/list", json!({}), &[]).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    // Another agent has its own budget.
    assert_eq!(
        e.modern(&e.writer, "tools/list", json!({}), &[])
            .await
            .status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn agents_never_sign_in_on_the_web_or_webdav() {
    let e = env();
    let a = e.st.db.user("writer").unwrap().unwrap();
    assert!(a.is_agent());
    assert!(matches!(
        crate::auth::apppass::create(&e.st.db, "writer", "dav", now()),
        Err(crate::auth::apppass::CreateError::Agent)
    ));
    // The web sign-in with an empty password.
    let web = crate::web::router(e.st.clone());
    let mut req = HttpRequest::builder()
        .method("POST")
        .uri("/.cc/login")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from("username=writer&password=&next=%2F"))
        .unwrap();
    req.extensions_mut().insert(ConnectInfo(SocketAddr::new(
        "127.0.0.1".parse().unwrap(),
        1,
    )));
    let res = web.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert!(res.headers().get("set-cookie").is_none());
}

// ---------------------------------------------------------------------------------------
// Browse + read

#[tokio::test]
async fn browse_respects_roles_hidden_and_internal_names() {
    let e = env();
    let v = e.ok(&e.reader, "volumes", json!({})).await;
    let names: Vec<&str> = v["volumes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Archive", "Docs", "Photos"], "no Secret, no Local");
    let docs = v["volumes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == "Docs")
        .unwrap();
    assert_eq!(docs["perms"], json!("r"));

    let l = e.ok(&e.reader, "list", json!({ "path": "/Docs" })).await;
    let names: Vec<&str> = l["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["sub", "bin.dat", "readme.txt"],
        "folders first, no dot-names"
    );
    assert_eq!(l["entries"][0]["type"], json!("folder"));
    assert_eq!(l["entries"][2]["size"], json!(12));

    // Paging.
    let p = e
        .ok(&e.reader, "list", json!({ "path": "/Docs", "limit": 2 }))
        .await;
    assert_eq!(
        (p["total"].clone(), p["next_offset"].clone()),
        (json!(3), json!(2))
    );
    let p = e
        .ok(
            &e.reader,
            "list",
            json!({ "path": "/docs/", "offset": 2, "limit": 2 }),
        )
        .await;
    assert_eq!(
        p["entries"][0]["name"],
        json!("readme.txt"),
        "volume name in any case"
    );
    assert!(p["next_offset"].is_null());

    for path in [
        "/Docs/.hidden",
        "/Docs/.dotfile",
        "/Docs/.hist",
        "/Docs/.hist/x",
        "/Secret",
        "/Secret/s.txt",
        "/Local",
        "/Nope",
        "/Docs/missing",
    ] {
        assert_eq!(
            e.err(&e.reader, "stat", json!({ "path": path })).await,
            "not_found",
            "{path}"
        );
    }
    assert_eq!(
        e.err(&e.reader, "list", json!({ "path": "/Docs/../Secret" }))
            .await,
        "bad_path"
    );
    assert_eq!(
        e.err(&e.reader, "list", json!({ "path": "/Docs/readme.txt" }))
            .await,
        "not_a_folder"
    );
    assert_eq!(
        e.err(&e.reader, "list", json!({ "pathh": "/Docs" })).await,
        "bad_arguments"
    );

    let s = e
        .ok(&e.reader, "stat", json!({ "path": "/Docs/readme.txt" }))
        .await;
    assert_eq!(
        (s["type"].clone(), s["size"].clone()),
        (json!("file"), json!(12))
    );
    assert_eq!(s["mime"], json!("text/plain"));
    let f = e
        .ok(
            &e.reader,
            "find",
            json!({ "path": "/Docs", "pattern": "*.TXT" }),
        )
        .await;
    assert_eq!(f["entries"].as_array().unwrap().len(), 1);
    let f = e
        .ok(
            &e.reader,
            "find",
            json!({ "path": "/Docs", "pattern": "dot" }),
        )
        .await;
    assert_eq!(f["total"], json!(0), "no hidden names");
    // "/" lists the volumes too.
    let root = e.ok(&e.reader, "list", json!({ "path": "/" })).await;
    assert_eq!(root["volumes"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn read_file_text_images_base64_and_pieces() {
    let e = env();
    let r = e
        .call(
            &e.reader,
            "read_file",
            json!({ "path": "/Docs/readme.txt" }),
        )
        .await;
    assert_eq!(r["content"][1]["text"], json!("hello world\n"), "{r}");
    assert_eq!(r["structuredContent"]["encoding"], json!("text"));

    let r = e
        .call(
            &e.reader,
            "read_file",
            json!({ "path": "/Docs/readme.txt", "offset": 6, "length": 3 }),
        )
        .await;
    assert_eq!(r["content"][1]["text"], json!("wor"));
    assert_eq!(r["structuredContent"]["next_offset"], json!(9));

    let r = e
        .call(&e.reader, "read_file", json!({ "path": "/Docs/bin.dat" }))
        .await;
    assert_eq!(r["structuredContent"]["encoding"], json!("base64"));
    assert_eq!(r["content"][1]["text"], json!("AAEC/wAH"));

    let r = e
        .call(&e.reader, "read_file", json!({ "path": "/Photos/pic.png" }))
        .await;
    assert_eq!(r["content"][1]["type"], json!("image"), "{r}");
    assert_eq!(r["content"][1]["mimeType"], json!("image/png"));
    let data = B64
        .decode(r["content"][1]["data"].as_str().unwrap())
        .unwrap();
    assert_eq!(data, tiny_png());
    let r = e
        .call(
            &e.reader,
            "read_file",
            json!({ "path": "/Photos/pic.png", "preview": true }),
        )
        .await;
    assert_eq!(r["isError"], json!(false), "{r}");
    assert_eq!(r["content"][1]["type"], json!("image"));

    // Larger than max_read_mib (1 MiB): pieces for text, preview for pictures.
    std::fs::write(e.root.join("docs/big.txt"), "x".repeat((1 << 20) + 10)).unwrap();
    let r = e
        .ok(&e.reader, "read_file", json!({ "path": "/Docs/big.txt" }))
        .await;
    assert_eq!(
        (r["length"].clone(), r["next_offset"].clone()),
        (json!(1 << 20), json!(1 << 20))
    );
    let mut big = tiny_png();
    big.resize((1 << 20) + 10, 0);
    std::fs::write(e.root.join("photos/big.png"), big).unwrap();
    assert_eq!(
        e.err(&e.reader, "read_file", json!({ "path": "/Photos/big.png" }))
            .await,
        "too_large"
    );
    assert_eq!(
        e.err(&e.reader, "read_file", json!({ "path": "/Docs/sub" }))
            .await,
        "not_a_file"
    );
    assert_eq!(
        e.err(
            &e.reader,
            "read_file",
            json!({ "path": "/Docs/readme.txt", "preview": true })
        )
        .await,
        "no_preview"
    );
    // Every call is audited, with the token and the bytes.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let audit = e.st.db.recent_audit(50).unwrap();
    let hit = audit
        .iter()
        .find(|a| a.action == "mcp_read_file" && a.target == "/Docs/readme.txt")
        .expect("audited");
    assert_eq!(hit.actor, "agent:reader");
    assert!(
        hit.detail.contains("token test") && hit.detail.contains("ok"),
        "{}",
        hit.detail
    );
    assert!(
        audit
            .iter()
            .any(|a| a.action == "mcp_read_file" && a.detail.contains("error too_large"))
    );
}

// ---------------------------------------------------------------------------------------
// Write: permissions, never overwrite, trash

#[tokio::test]
async fn reader_sees_and_may_use_no_write_tools() {
    let e = env();
    let names = e.tool_names(&e.reader).await;
    for t in [
        "write_file",
        "mkdir",
        "delete",
        "move",
        "share_create",
        "health_summary",
    ] {
        assert!(!names.contains(&t.to_string()), "{t}: {names:?}");
    }
    let code = e
        .err(
            &e.reader,
            "write_file",
            json!({ "path": "/Docs/x.txt", "content": "x" }),
        )
        .await;
    assert_eq!(code, "denied");
    assert!(e.read("docs/x.txt").is_none());
}

#[tokio::test]
async fn permission_matrix() {
    let e = env();
    // writer: Docs rwmds, Photos rw, Local rwmds, Archive read-only volume, no Secret.
    let w = |path: &str| json!({ "path": path, "content": "data" });
    assert_eq!(
        e.ok(&e.writer, "write_file", w("/Docs/new.txt")).await["written"],
        json!(true)
    );
    assert_eq!(
        e.ok(&e.writer, "write_file", w("/Photos/new.txt")).await["written"],
        json!(true)
    );
    assert_eq!(
        e.err(&e.writer, "write_file", w("/Archive/new.txt")).await,
        "denied"
    );
    assert_eq!(
        e.err(&e.writer, "write_file", w("/Secret/new.txt")).await,
        "not_found"
    );
    assert_eq!(
        e.err(&e.writer, "write_file", w("/Docs/.env")).await,
        "denied",
        "dot-names"
    );
    assert_eq!(
        e.err(&e.writer, "write_file", w("/Docs/.chestcraft-trash/x"))
            .await,
        "not_found"
    );
    assert_eq!(
        e.err(&e.writer, "write_file", w("/Docs/nope/x.txt")).await,
        "not_found"
    );
    // Photos has no m and no d.
    assert_eq!(
        e.err(
            &e.writer,
            "rename",
            json!({ "path": "/Photos/pic.png", "new_name": "p.png" })
        )
        .await,
        "denied"
    );
    assert_eq!(
        e.err(&e.writer, "delete", json!({ "path": "/Photos/pic.png" }))
            .await,
        "denied"
    );
    assert_eq!(
        e.err(&e.writer, "delete", json!({ "path": "/Archive/old.txt" }))
            .await,
        "denied"
    );
    assert_eq!(
        e.err(&e.writer, "delete", json!({ "path": "/Docs" })).await,
        "bad_path"
    );
    // A move between volumes needs m on the source and w on the target.
    assert_eq!(
        e.err(
            &e.writer,
            "move",
            json!({ "path": "/Photos/pic.png", "to": "/Docs" })
        )
        .await,
        "denied"
    );
    let m = e
        .ok(
            &e.writer,
            "move",
            json!({ "path": "/Docs/new.txt", "to": "/Photos" }),
        )
        .await;
    assert_eq!(m["path"], json!("/Photos/new (1).txt"), "keeps both: {m}");
    assert!(e.read("docs/new.txt").is_none());
    assert_eq!(e.read("photos/new.txt").unwrap(), b"data");
    // mkdir.
    assert_eq!(
        e.ok(&e.writer, "mkdir", json!({ "path": "/Docs/made" }))
            .await["created"],
        json!(true)
    );
    assert_eq!(
        e.ok(&e.writer, "mkdir", json!({ "path": "/Docs/made" }))
            .await["existed"],
        json!(true)
    );
    assert_eq!(
        e.err(&e.writer, "mkdir", json!({ "path": "/Docs/readme.txt" }))
            .await,
        "exists"
    );
    assert_eq!(
        e.err(&e.writer, "mkdir", json!({ "path": "/Archive/x" }))
            .await,
        "denied"
    );
}

#[tokio::test]
async fn writes_never_overwrite() {
    let e = env();
    let r = e
        .ok(
            &e.writer,
            "write_file",
            json!({ "path": "/Docs/readme.txt", "content": "NEW" }),
        )
        .await;
    assert_eq!(r["path"], json!("/Docs/readme (1).txt"));
    assert_eq!(r["renamed"], json!(true));
    assert_eq!(
        e.read("docs/readme.txt").unwrap(),
        b"hello world\n",
        "original untouched"
    );
    assert_eq!(e.read("docs/readme (1).txt").unwrap(), b"NEW");
    let mode = std::os::unix::fs::PermissionsExt::mode(
        &std::fs::metadata(e.root.join("docs/readme (1).txt"))
            .unwrap()
            .permissions(),
    );
    assert_eq!(mode & 0o777, 0o644);

    let r = e
        .ok(
            &e.writer,
            "write_file",
            json!({ "path": "/Docs/readme.txt", "content": "SKIP", "on_clash": "skip" }),
        )
        .await;
    assert_eq!(r["written"], json!(false));
    assert_eq!(e.read("docs/readme.txt").unwrap(), b"hello world\n");
    assert_eq!(
        e.err(
            &e.writer,
            "write_file",
            json!({ "path": "/Docs/x", "content": "x", "on_clash": "replace" })
        )
        .await,
        "bad_arguments",
        "no replace"
    );

    // base64 and the size limit.
    let r = e
        .ok(
            &e.writer,
            "write_file",
            json!({ "path": "/Docs/b.bin", "content": "AAEC/w==", "encoding": "base64" }),
        )
        .await;
    assert_eq!(r["size"], json!(4));
    assert_eq!(e.read("docs/b.bin").unwrap(), [0, 1, 2, 255]);
    let big = "x".repeat((1 << 20) + 1);
    assert_eq!(
        e.err(
            &e.writer,
            "write_file",
            json!({ "path": "/Docs/big", "content": big })
        )
        .await,
        "too_large"
    );

    // copy and rename keep both too.
    let c = e
        .ok(
            &e.writer,
            "copy",
            json!({ "path": "/Docs/sub/a.txt", "to": "/Docs/sub" }),
        )
        .await;
    assert_eq!(c["path"], json!("/Docs/sub/a (1).txt"));
    let s = e
        .ok(
            &e.writer,
            "copy",
            json!({ "path": "/Docs/sub/a.txt", "to": "/Docs/sub", "on_clash": "skip" }),
        )
        .await;
    assert_eq!(s["done"], json!(false));
    let r = e
        .ok(
            &e.writer,
            "rename",
            json!({ "path": "/Docs/sub/a (1).txt", "new_name": "a.txt" }),
        )
        .await;
    assert_eq!(r["path"], json!("/Docs/sub/a (2).txt"));
    assert_eq!(e.read("docs/sub/a.txt").unwrap(), b"a");
    // No upload leftovers.
    let leftovers: Vec<_> = std::fs::read_dir(e.root.join("docs/.chestcraft-uploads"))
        .map(|d| d.flatten().collect())
        .unwrap_or_default();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[tokio::test]
async fn delete_goes_to_the_trash_and_restores_without_overwriting() {
    let e = env();
    let d = e
        .ok(&e.writer, "delete", json!({ "path": "/Docs/readme.txt" }))
        .await;
    assert_eq!(d["trashed"], json!(true));
    assert!(e.read("docs/readme.txt").is_none());
    let id = d["trash_id"].as_str().unwrap().to_string();
    let item = e.st.db.trash_item(&id).unwrap().unwrap();
    assert_eq!(item.deleted_by, "agent:writer");
    let trashed = e
        .root
        .join(format!("docs/.chestcraft-trash/{id}/readme.txt"));
    assert_eq!(std::fs::read(trashed).unwrap(), b"hello world\n");

    let t = e.ok(&e.writer, "trash_list", json!({})).await;
    assert_eq!(t["items"][0]["path"], json!("/Docs/readme.txt"));
    // Another agent doesn't see or restore it.
    assert_eq!(
        e.ok(&e.reader, "volumes", json!({})).await["volumes"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    let other = e.st.db.trash_items().unwrap();
    assert_eq!(other.len(), 1);

    // The name is taken again: it comes back as "(restored)".
    std::fs::write(e.root.join("docs/readme.txt"), "newer").unwrap();
    let r = e.ok(&e.writer, "trash_restore", json!({ "id": id })).await;
    assert_eq!(r["restored"], json!("/Docs/readme (restored).txt"));
    assert_eq!(e.read("docs/readme.txt").unwrap(), b"newer");
    assert_eq!(
        e.read("docs/readme (restored).txt").unwrap(),
        b"hello world\n"
    );
    assert_eq!(
        e.err(&e.writer, "trash_restore", json!({ "id": id })).await,
        "not_found"
    );
}

// ---------------------------------------------------------------------------------------
// Shares

#[tokio::test]
async fn agent_shares_follow_the_rules_and_alert() {
    let e = env();
    let s = e
        .ok(
            &e.writer,
            "share_create",
            json!({ "path": "/Docs/sub", "mode": "view_upload" }),
        )
        .await;
    let id = s["id"].as_str().unwrap().to_string();
    assert!(
        s["link"]
            .as_str()
            .unwrap()
            .starts_with("https://files.example.org/s/")
    );
    assert!(s["password"].as_str().unwrap().len() >= 8);
    assert!(s["expires"].is_string(), "7 days by default");
    let share = e.st.db.share(&id).unwrap().unwrap();
    assert_eq!(
        (share.owner.as_str(), share.mode.as_str()),
        ("writer", "view_upload")
    );
    assert!(crate::shares::verify_password(
        s["password"].as_str().unwrap(),
        &share.password_hash,
        share.passphrase
    ));

    // Alerted (logged as an event).
    let mut alerted = false;
    for _ in 0..50 {
        let log: Vec<LogEntry> = e.st.db.alert_log(20).unwrap();
        if log.iter().any(|l| l.key == format!("agent_share:{id}")) {
            alerted = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(alerted, "share alert");

    // Same rules as the UI.
    assert_eq!(
        e.err(&e.writer, "share_create", json!({ "path": "/Local/l.txt" }))
            .await,
        "denied",
        "home-only volume"
    );
    assert_eq!(
        e.err(
            &e.writer,
            "share_create",
            json!({ "path": "/Photos/pic.png" })
        )
        .await,
        "denied",
        "no s on Photos"
    );
    assert_eq!(
        e.err(
            &e.writer,
            "share_create",
            json!({ "path": "/Docs/readme.txt", "mode": "upload" })
        )
        .await,
        "bad_arguments"
    );
    assert_eq!(
        e.err(
            &e.writer,
            "share_create",
            json!({ "path": "/Docs/readme.txt", "password": "short" })
        )
        .await,
        "bad_arguments"
    );

    let l = e.ok(&e.writer, "share_list", json!({})).await;
    assert_eq!(l["shares"][0]["status"], json!("active"));
    assert!(l["shares"][0].get("password").is_none());
    // Only own shares.
    assert_eq!(
        e.err(&e.reader, "share_revoke", json!({ "id": id })).await,
        "denied",
        "no s"
    );
    let r = e.ok(&e.writer, "share_revoke", json!({ "id": id })).await;
    assert_eq!(r["revoked"], json!(true));
    assert!(e.st.db.share(&id).unwrap().unwrap().revoked.is_some());

    // A disabled agent's shares pause.
    let s2 = e
        .ok(
            &e.writer,
            "share_create",
            json!({ "path": "/Docs/readme.txt" }),
        )
        .await;
    e.st.db
        .update_user("writer", |u| u.disabled = true)
        .unwrap();
    let sh = e.st.db.share(s2["id"].as_str().unwrap()).unwrap().unwrap();
    let (status, _) =
        crate::shares::status(&e.st.cfg, &e.st.volumes, &e.st.db, &sh, now()).unwrap();
    assert_eq!(
        status,
        crate::shares::Status::Paused(crate::shares::Pause::Owner)
    );
}

#[tokio::test]
async fn shares_follow_agent_moves() {
    let e = env();
    let s = e
        .ok(&e.writer, "share_create", json!({ "path": "/Docs/sub" }))
        .await;
    e.ok(
        &e.writer,
        "rename",
        json!({ "path": "/Docs/sub", "new_name": "renamed" }),
    )
    .await;
    let share = e.st.db.share(s["id"].as_str().unwrap()).unwrap().unwrap();
    assert_eq!(RelPath::parse(&share.path).unwrap().as_str(), "renamed");
}

// ---------------------------------------------------------------------------------------
// Health

#[tokio::test]
async fn health_tools_only_for_health_roles() {
    let e = env();
    assert_eq!(
        e.err(&e.reader, "health_summary", json!({})).await,
        "denied"
    );
    assert_eq!(
        e.err(&e.writer, "pool", json!({ "name": "tank" })).await,
        "denied"
    );
    let names = e.tool_names(&e.ops).await;
    assert!(names.contains(&"health_summary".to_string()) && names.contains(&"disk".to_string()));
    assert!(
        !names.contains(&"write_file".to_string()),
        "ops has no write rights"
    );

    assert_eq!(
        e.err(&e.ops, "pool", json!({ "name": "tank" })).await,
        "no_data"
    );
    let mut snap = Snapshot::empty(now());
    snap.pools = Probe::Ok(
        zpool::parse(
            include_bytes!("../../fixtures/zpool-status-raidz1.json"),
            include_bytes!("../../fixtures/zpool-list.json"),
        )
        .unwrap(),
    );
    e.st.health.set_latest(snap);
    let h = e.ok(&e.ops, "health_summary", json!({})).await;
    assert_eq!(h["pools"][0]["name"], json!("tank"), "{h}");
    assert!(h["data_age_secs"].as_i64().unwrap() < 60);
    let p = e.ok(&e.ops, "pool", json!({ "name": "tank" })).await;
    assert!(!p["vdevs"].as_array().unwrap().is_empty(), "{p}");
    assert_eq!(
        e.err(&e.ops, "pool", json!({ "name": "nope" })).await,
        "not_found"
    );
    let a = e.ok(&e.ops, "alerts", json!({})).await;
    assert!(a["recent"].is_array());
}

// ---------------------------------------------------------------------------------------
// Unknown tools and bad requests

#[tokio::test]
async fn unknown_tools_and_header_mismatch() {
    let e = env();
    let r = e
        .modern(
            &e.reader,
            "tools/call",
            json!({ "name": "rm_rf", "arguments": {} }),
            &[],
        )
        .await;
    let v = r.json();
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Unknown tool"),
        "{v}"
    );
    // The body names another tool than the Mcp-Name header.
    let r = e
        .modern(
            &e.reader,
            "tools/call",
            json!({ "name": "list", "arguments": { "path": "/Docs" } }),
            &[("mcp-name", "volumes")],
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
}
