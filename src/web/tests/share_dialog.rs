//! The share dialog (review Q19, Q39): the JSON side of `/.cc/shares/new` (target, modes
//! and passphrase; the created share with link, password and messages; errors per field),
//! `no-store` on everything that carries a password, CSRF, the page as the way without
//! script, and the markup the dialog and the disabled multi-select Share need.

use super::*;

const ACCEPT_JSON: (&str, &str) = ("accept", "application/json");

impl Env {
    /// POST the create form as the dialog does (`Accept: application/json`).
    async fn share_json(&self, s: &Login, pairs: &[(&str, &str)], extra: &[(&str, &str)]) -> Res {
        let mut all = vec![("csrf", s.csrf.as_str())];
        all.extend_from_slice(pairs);
        let mut headers = vec![FORM, ACCEPT_JSON, ("cookie", s.cookie.as_str())];
        headers.extend_from_slice(extra);
        self.send_body("POST", "/.cc/shares/new", "127.0.0.1", &headers, form(&all))
            .await
    }

    fn share_count(&self) -> usize {
        self.state.db.shares().unwrap().len()
    }
}

fn json_of(r: &Res) -> serde_json::Value {
    serde_json::from_slice(&r.body).unwrap_or_else(|_| panic!("not JSON: {}", r.text()))
}

/// What "Create link" sends with the defaults untouched: no passphrase (the server makes
/// one), view, 30 days.
const DEFAULTS: [(&str, &str); 4] = [
    ("mode", "view"),
    ("expiry", "30d"),
    ("password_kind", "generated"),
    ("op", "create"),
];

#[tokio::test]
async fn dialog_asks_for_the_target_and_creates_in_one_step() {
    let e = env();
    e.put("medien/Ordner/x.txt", b"x");
    let jana = e.session("jana", "teiler").await;
    let c = [("cookie", jana.cookie.as_str()), ACCEPT_JSON];

    // Step 1: the target, the modes allowed here, a passphrase for "Change".
    let r = e
        .get_h("/.cc/shares/new?vol=Medien&path=Ordner%2Fx.txt", &c)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.header("cache-control"), "no-store");
    let info = json_of(&r);
    assert_eq!(info["name"], "x.txt");
    assert_eq!(info["location"], "Medien/Ordner");
    assert_eq!(info["dir"], false);
    // A file takes no uploads.
    assert_eq!(info["modes"], serde_json::json!(["view"]));
    assert!(info["passphrase"].as_str().unwrap().contains('-'), "{info}");
    let info = json_of(&e.get_h("/.cc/shares/new?vol=Medien&path=Ordner", &c).await);
    assert_eq!(
        (&info["location"], &info["dir"], &info["modes"]),
        (
            &serde_json::json!("Medien"),
            &serde_json::json!(true),
            &serde_json::json!(["view", "view_upload", "upload"])
        )
    );

    // "Create link" with the defaults.
    let before = e.share_count();
    let r = e
        .share_json(
            &jana,
            &[&[("vol", "Medien"), ("path", "Ordner")][..], &DEFAULTS[..]].concat(),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.header("cache-control"), "no-store");
    assert_eq!(e.share_count(), before + 1);
    let made = json_of(&r);
    let link = made["link"].as_str().unwrap();
    let password = made["password"].as_str().unwrap();
    let id = link.strip_prefix("https://files.example.org/s/").unwrap();
    assert_eq!(made["details_href"], format!("/.cc/shares/{id}"));
    assert!(password.split('-').count() >= 3, "{made}");
    assert_eq!(made["lang"], "en");
    // The message in both built-in languages, each with link, password and date.
    let (en, de) = (
        made["message"]["en"].as_str().unwrap(),
        made["message"]["de"].as_str().unwrap(),
    );
    assert!(
        en.contains(link) && en.contains(&format!("Password: {password}")),
        "{en}"
    );
    assert!(
        de.contains(link) && de.contains(&format!("Passwort: {password}")),
        "{de}"
    );
    let until = made["expires_at"].as_i64().unwrap();
    assert!((until - now() - 30 * 86_400).abs() < 60, "{made}");
    assert!(!made["expires"].as_str().unwrap().is_empty());
    assert!(
        made["badge"].as_str().unwrap().starts_with("Active share"),
        "{made}"
    );
    let share = e.share(id);
    assert_eq!((share.mode.as_str(), share.is_dir), ("view", true));
    e.audited(&["share_created"]).await;
    // The password opens the share.
    e.visit(id, password).await;

    // Options: a passphrase from "Change" is used as shown, an upload mode keeps its limit.
    let r = e
        .share_json(
            &jana,
            &[
                ("vol", "Medien"),
                ("path", "Ordner"),
                ("mode", "view_upload"),
                ("expiry", "never"),
                ("password_kind", "generated"),
                ("passphrase", "eins-zwei-drei-vier"),
                ("note", "Für die Fotos"),
                ("max_mib", "50"),
            ],
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let made = json_of(&r);
    assert_eq!(made["password"], "eins-zwei-drei-vier");
    assert_eq!(made["expires_at"], serde_json::Value::Null);
    assert_eq!(made["badge"], "Active share without expiry");
    let id = made["link"].as_str().unwrap().rsplit('/').next().unwrap();
    let share = e.share(id);
    assert_eq!(
        (share.mode.as_str(), share.max_file, share.note.as_str()),
        ("view_upload", Some(50 << 20), "Für die Fotos")
    );

    // "Another passphrase" asked by POST answers only the passphrase.
    let before = e.share_count();
    let r = e
        .share_json(
            &jana,
            &[("vol", "Medien"), ("path", "Ordner"), ("op", "regenerate")],
            &[],
        )
        .await;
    assert_eq!(
        (r.status, r.header("cache-control")),
        (StatusCode::OK, "no-store")
    );
    let pp = json_of(&r);
    assert!(
        pp["passphrase"].as_str().is_some() && pp.get("link").is_none(),
        "{pp}"
    );
    assert_eq!(e.share_count(), before);
}

#[tokio::test]
async fn dialog_errors_come_per_field_and_create_nothing() {
    let e = env();
    e.put("medien/Ordner/x.txt", b"x");
    let jana = e.session("jana", "teiler").await;
    let target = [("vol", "Medien"), ("path", "Ordner")];
    let before = e.share_count();
    let errors = |r: &Res| json_of(r)["errors"].clone();

    let short = [
        &target[..],
        &[
            ("mode", "view"),
            ("expiry", "30d"),
            ("password_kind", "own"),
            ("own_password", "kurz"),
        ][..],
    ]
    .concat();
    let r = e.share_json(&jana, &short, &[]).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.header("cache-control"), "no-store");
    assert_eq!(
        errors(&r)["own_password"],
        "Use a password with at least 8 characters."
    );

    let size = [&target[..], &[("mode", "upload"), ("max_mib", "0")][..]].concat();
    let r = e.share_json(&jana, &size, &[]).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        errors(&r)["max_mib"]
            .as_str()
            .unwrap()
            .contains("whole number"),
        "{}",
        r.text()
    );

    let long = "x".repeat(crate::shares::MAX_NOTE_CHARS + 1);
    let note = [&target[..], &[("note", long.as_str())][..]].concat();
    let r = e.share_json(&jana, &note, &[]).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(errors(&r)["note"], "The note is too long.");

    // Gone target, no rights: a form-level message.
    let r = e
        .share_json(&jana, &[("vol", "Medien"), ("path", "fehlt.txt")], &[])
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert!(
        errors(&r)["form"]
            .as_str()
            .unwrap()
            .contains("doesn't exist")
    );

    // CSRF and origin are checked like the form's.
    let bad = Login {
        cookie: jana.cookie.clone(),
        csrf: "falsch".into(),
    };
    let r = e.share_json(&bad, &target, &[]).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(errors(&r)["form"].as_str().is_some(), "{}", r.text());
    let r = e
        .share_json(&jana, &target, &[("origin", "https://evil.example.net")])
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(e.share_count(), before);

    // Without an account: 401 for the script (it opens the page, which signs in).
    let r = e
        .send_body(
            "POST",
            "/.cc/shares/new",
            "127.0.0.1",
            &[FORM, ACCEPT_JSON],
            form(&target),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = e
        .get_h("/.cc/shares/new?vol=Medien&path=Ordner", &[ACCEPT_JSON])
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);

    // Asking for a target one may not share: the reason, for the page fallback.
    let leser = e.session("mika", "leser").await;
    let r = e
        .get_h(
            "/.cc/shares/new?vol=Medien&path=Ordner",
            &[("cookie", &leser.cookie), ACCEPT_JSON],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(json_of(&r)["error"].as_str().is_some());
    let admin = e.session("Admin", "admin").await;
    e.put("backups/dump.bin", b"d");
    let r = e
        .get_h(
            "/.cc/shares/new?vol=Backups&path=dump.bin",
            &[("cookie", &admin.cookie), ACCEPT_JSON],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(
        json_of(&r)["error"]
            .as_str()
            .unwrap()
            .contains("only reachable at home")
    );
}

/// Without script the page stays the way to share, and its answer isn't cached either.
#[tokio::test]
async fn page_fallback_still_creates_without_caching() {
    let e = env();
    e.put("medien/Ordner/x.txt", b"x");
    let jana = e.session("jana", "teiler").await;
    let page = e
        .get_h(
            "/.cc/shares/new?vol=Medien&path=Ordner",
            &[("cookie", &jana.cookie)],
        )
        .await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.text().contains("value=\"create\""), "{}", page.text());
    let r = e
        .form_post(
            &jana,
            "/.cc/shares/new",
            &[&[("vol", "Medien"), ("path", "Ordner")][..], &DEFAULTS[..]].concat(),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.header("cache-control"), "no-store");
    assert!(r.text().contains("id=\"secret-password\""));
}

#[tokio::test]
async fn browser_and_viewer_carry_the_dialog() {
    let e = env();
    e.put("medien/Ordner/x.txt", b"x");
    e.put("medien/b.txt", b"b");
    let jana = e.session("jana", "teiler").await;
    let t = e
        .get_h("/Medien/", &[("cookie", &jana.cookie)])
        .await
        .text();
    for needle in [
        r#"data-js="share-dialog""#,
        r#"data-js="share-form""#,
        r#"action="/.cc/shares/new""#,
        r#"data-js="share-create""#,
        r#"data-js="share-change""#,
        r#"data-js="share-copy-message""#,
        r#"data-js="share-lang""#,
        r#"template data-js="share-badge""#,
        "share.js",
        "copy.js",
        // The selection's Share and the hint that explains it with several items (Q39).
        r#"data-js="share""#,
        r#"id="share-multi-hint""#,
        "Put them in a folder to share them together.",
        // Each row still links the page (the row menu's link, the way without script).
        r#"data-share="/.cc/shares/new?vol=Medien&amp;path=b.txt""#,
    ] {
        assert!(t.contains(needle), "{needle}: {t}");
    }
    // The dialog's choices: every mode (the script hides what the target can't take),
    // 30 days and the generated passphrase preset.
    assert!(t.contains(r#"name="mode" value="view" checked"#), "{t}");
    assert!(t.contains(r#"name="expiry" value="30d" checked"#), "{t}");
    assert!(
        t.contains(r#"name="password_kind" value="generated" checked"#),
        "{t}"
    );
    // German hint and dialog strings.
    let de = e
        .get_h(
            "/Medien/",
            &[("cookie", &jana.cookie), ("accept-language", "de")],
        )
        .await
        .text();
    assert!(
        de.contains("Leg sie in einen Ordner, um sie zusammen zu teilen.")
            && de.contains("Link erstellen")
            && de.contains("Als Nachricht kopieren"),
        "{de}"
    );

    // The viewer's Share stays a link to the page; the dialog comes along for the script.
    let v = e
        .get_h("/Medien/b.txt?view", &[("cookie", &jana.cookie)])
        .await
        .text();
    assert!(
        v.contains(r#"href="/.cc/shares/new?vol=Medien&amp;path=b.txt" data-js="share-open""#),
        "{v}"
    );
    assert!(
        v.contains(r#"data-js="share-dialog""#) && v.contains("share.js"),
        "{v}"
    );

    // No right to share: no dialog, no script.
    let leser = e.session("mika", "leser").await;
    for uri in ["/Medien/", "/Medien/b.txt?view"] {
        let t = e.get_h(uri, &[("cookie", &leser.cookie)]).await.text();
        assert!(
            !t.contains("share-dialog")
                && !t.contains("share.js")
                && !t.contains("share-multi-hint"),
            "{uri}: {t}"
        );
    }
    // Home-only volume: the reason as before, nothing to open.
    let admin = e.session("Admin", "admin").await;
    e.put("backups/dump.bin", b"d");
    let t = e
        .get_h("/Backups/", &[("cookie", &admin.cookie)])
        .await
        .text();
    assert!(
        t.contains("only reachable at home") && !t.contains("share-dialog"),
        "{t}"
    );
}
