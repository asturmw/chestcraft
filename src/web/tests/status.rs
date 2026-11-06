//! `/healthz`, the status API and its tokens.

use super::*;
use crate::auth::apitoken;

fn json(r: &Res) -> serde_json::Value {
    serde_json::from_slice(&r.body).unwrap_or_else(|e| panic!("{e}: {}", r.text()))
}

impl Env {
    fn token(&self, label: &str) -> String {
        apitoken::create(&self.state.db, label, "cli", now())
            .unwrap()
            .0
    }

    async fn status_from(&self, peer: &str, extra: &[(&str, &str)], token: Option<&str>) -> Res {
        let auth = token.map(|t| format!("Bearer {t}"));
        let mut headers: Vec<(&str, &str)> = extra.to_vec();
        if let Some(a) = &auth {
            headers.push(("authorization", a));
        }
        self.send("GET", "/api/v1/status", peer, &headers).await
    }

    async fn volumes_page(&self, s: &Login) -> String {
        self.get_h("/.cc/admin/volumes", &[("cookie", &s.cookie)])
            .await
            .text()
    }

    /// Healthy: a heartbeat and a fresh snapshot.
    fn healthy(&self) {
        self.state.health.db_ok(now());
        self.state.health.set_latest(Snapshot::empty(now()));
    }
}

#[tokio::test]
async fn healthz_names_what_is_broken() {
    let e = env();
    let check = |r: Res, status: StatusCode, body: &str| {
        assert_eq!((r.status, r.text().as_str()), (status, body));
        assert_eq!(r.header("cache-control"), "no-store");
        assert!(r.header("content-type").starts_with("text/plain"));
    };
    // Just started: nothing read yet.
    check(
        e.get("/healthz").await,
        StatusCode::SERVICE_UNAVAILABLE,
        "starting\n",
    );
    // Still nothing after the grace time.
    e.state.health.set_started(now() - 1000);
    check(
        e.get("/healthz").await,
        StatusCode::SERVICE_UNAVAILABLE,
        "db monitor\n",
    );

    e.healthy();
    check(e.get("/healthz").await, StatusCode::OK, "ok\n");
    // From the internet, with a stale cookie, as HEAD: the same answer, no session lookup.
    let r = e
        .send(
            "GET",
            "/healthz",
            "192.0.2.10",
            &[
                ("cf-connecting-ip", "203.0.113.9"),
                ("cookie", "cc_session=0123456789abcdef0123456789abcdef"),
            ],
        )
        .await;
    check(r, StatusCode::OK, "ok\n");
    assert_eq!(
        e.send("HEAD", "/healthz", "127.0.0.1", &[]).await.status,
        StatusCode::OK
    );

    // The monitor loop hangs: the snapshot ages.
    e.state.health.set_latest(Snapshot::empty(now() - 400));
    check(
        e.get("/healthz").await,
        StatusCode::SERVICE_UNAVAILABLE,
        "monitor\n",
    );
    // The database stops taking writes.
    e.state.health.db_ok(now() - 400);
    check(
        e.get("/healthz").await,
        StatusCode::SERVICE_UNAVAILABLE,
        "db monitor\n",
    );
    e.healthy();

    // A source fails (zpool hangs) → collector; a degraded pool alone is not a 503.
    let mut snap = Snapshot::empty(now());
    snap.pools = Probe::Failed("zpool status: timed out".into());
    tick_with(&e, snap);
    check(
        e.get("/healthz").await,
        StatusCode::SERVICE_UNAVAILABLE,
        "collector\n",
    );
    tick_with(&e, degraded_snapshot(now()));
    check(e.get("/healthz").await, StatusCode::OK, "ok\n");
    // The SMART collector's file goes stale.
    let mut snap = degraded_snapshot(now());
    snap.smart = SmartFile::Ok {
        collected: now() - 3 * 3600,
    };
    tick_with(&e, snap);
    check(
        e.get("/healthz").await,
        StatusCode::SERVICE_UNAVAILABLE,
        "collector\n",
    );
}

#[tokio::test]
async fn status_api_wants_a_token_from_the_lan() {
    let e = env();
    let token = e.token("watchdog");
    let remote = [("cf-connecting-ip", "203.0.113.9")];
    // From the internet the API doesn't exist, token or not.
    let r = e.status_from("192.0.2.10", &remote, Some(&token)).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = e.status_from("203.0.113.50", &[], Some(&token)).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    let r = e.status_from("127.0.0.1", &[], None).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(r.header("www-authenticate"), "Bearer realm=\"chestcraft\"");
    assert_eq!(json(&r)["error"], "token required");
    // A session cookie or Basic auth is no substitute.
    let admin = e.session("Admin", "admin").await;
    let r = e
        .send(
            "GET",
            "/api/v1/status",
            "127.0.0.1",
            &[("cookie", &admin.cookie)],
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);

    // Wrong tokens slow the address down.
    let wrong = apitoken::generate();
    for _ in 0..crate::auth::backoff::FREE_TRIES {
        let r = e.status_from("192.168.1.66", &[], Some(&wrong)).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
        assert_eq!(json(&r)["error"], "invalid token");
    }
    let r = e.status_from("192.168.1.66", &[], Some(&token)).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(r.header("retry-after").parse::<i64>().unwrap() > 0);
    e.audited(&["api_token_failed"]).await;

    // The right token from elsewhere on the LAN.
    let r = e.status_from("192.168.1.20", &[], Some(&token)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.header("cache-control"), "no-store");
    assert!(r.header("content-type").starts_with("application/json"));
    let v = json(&r);
    assert_eq!(v["api"], 1);
    assert_eq!(v["version"], crate::VERSION);
    assert_eq!(v["healthy"], false);
    assert_eq!(v["problems"], serde_json::json!(["starting"]));
    assert_eq!(v["verdict"], "stale");
    assert!(v["as_of"].is_null() && v["pools"] == serde_json::json!([]));
    let (_, rec) = e.state.db.api_tokens().unwrap().pop().unwrap();
    assert_eq!(
        (rec.last_ip.as_deref(), rec.last_used.is_some()),
        (Some("192.168.1.20"), true)
    );

    // Revoked: gone.
    apitoken::revoke(&e.state.db, "watchdog").unwrap().unwrap();
    let r = e.status_from("192.168.1.20", &[], Some(&token)).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn status_reports_the_snapshot() {
    let e = env();
    let token = e.token("watchdog");
    e.healthy();
    tick_with(&e, degraded_snapshot(now()));
    let r = e.status_from("127.0.0.1", &[], Some(&token)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let v = json(&r);
    assert_eq!(
        (v["healthy"].clone(), v["verdict"].clone()),
        (true.into(), "critical".into())
    );
    assert_eq!(v["problems"], serde_json::json!([]));
    assert!(v["as_of"].as_i64().unwrap() > 0);
    assert_eq!(v["sources"]["pools"]["state"], "ok");
    assert_eq!(v["sources"]["datasets"]["state"], "absent");
    assert_eq!(v["sources"]["smart"]["state"], "ok");

    let alerts = v["alerts"].as_array().unwrap();
    assert!(
        alerts.iter().any(|a| a["severity"] == "critical"
            && a["title"]
                .as_str()
                .unwrap()
                .starts_with("Pool fast is DEGRADED")),
        "{alerts:?}"
    );
    let fast = v["pools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "fast")
        .unwrap();
    assert_eq!(fast["state"], "DEGRADED");
    assert_eq!(
        (fast["read_errors"].clone(), fast["cksum_errors"].clone()),
        (3.into(), 12.into())
    );
    assert!(
        fast["vdevs_not_online"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n.as_str().unwrap().starts_with("mirror")),
        "{fast}"
    );
    assert_eq!(fast["scan"]["function"], "RESILVER");

    let md = &v["md"][0];
    assert_eq!(
        (md["name"].clone(), md["degraded"].clone()),
        ("md0".into(), false.into())
    );
    let disk = &v["disks"][0];
    assert_eq!(disk["label"], "Bay 3");
    assert_eq!(disk["kind"], "hdd");
    assert_eq!(disk["pools"], serde_json::json!(["fast"]));
    assert_eq!(disk["smart"], "ok");
    assert_eq!(disk["passed"], false);
    assert_eq!(disk["realloc"], 184);
    assert_eq!(v["host"]["root_fs"]["percent"], 5);
    assert_eq!(v["app"]["sessions"], 0);
    assert_eq!(v["channels"], serde_json::json!([]));
}

#[tokio::test]
async fn admin_creates_and_revokes_tokens_on_the_volumes_page() {
    let e = env();
    let admin = e.session("Admin", "admin").await;
    let jana = e.session("jana", "familie").await;
    let r = e
        .form_post(
            &jana,
            "/.cc/admin/api-tokens",
            &[("op", "add"), ("label", "x")],
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = e
        .send_body(
            "POST",
            "/.cc/admin/api-tokens",
            "127.0.0.1",
            &[FORM, ("cookie", &admin.cookie)],
            form(&[("csrf", "nope"), ("op", "add"), ("label", "x")]),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(e.state.db.api_tokens().unwrap().is_empty());

    assert!(e.volumes_page(&admin).await.contains("No tokens yet."));

    let r = e
        .form_post(
            &admin,
            "/.cc/admin/api-tokens",
            &[("op", "add"), ("label", " Watchdog ")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let token = attr_after(&r.text(), "id=\"secret-token\"", "value");
    assert!(apitoken::plausible(&token), "{token}");
    assert!(r.text().contains("shown only once"));
    assert_eq!(r.header("cache-control"), "no-store");

    let r = e
        .form_post(
            &admin,
            "/.cc/admin/api-tokens",
            &[("op", "add"), ("label", "watchdog")],
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(r.text().contains("already a token with this label"));
    let r = e
        .form_post(
            &admin,
            "/.cc/admin/api-tokens",
            &[("op", "add"), ("label", "")],
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);

    let t = e.volumes_page(&admin).await;
    assert!(t.contains("Watchdog") && t.contains("Never"), "{t}");
    let r = e.status_from("127.0.0.1", &[], Some(&token)).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(e.volumes_page(&admin).await.contains("from 127.0.0.1"));

    let id = e.state.db.api_tokens().unwrap()[0].1.id.clone();
    let r = e
        .form_post(
            &admin,
            "/.cc/admin/api-tokens",
            &[("op", "revoke"), ("id", &id)],
        )
        .await;
    assert_eq!(
        (r.status, r.header("location")),
        (StatusCode::SEE_OTHER, "/.cc/admin/volumes#api-tokens")
    );
    let r = e
        .form_post(
            &admin,
            "/.cc/admin/api-tokens",
            &[("op", "revoke"), ("id", &id)],
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        e.status_from("127.0.0.1", &[], Some(&token)).await.status,
        StatusCode::UNAUTHORIZED
    );
    e.audited(&["api_token_created", "api_token_revoked"]).await;
}
