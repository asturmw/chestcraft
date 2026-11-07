//! Rename, move and copy through the JSON API.

use super::*;

fn json(r: &Res) -> serde_json::Value {
    serde_json::from_slice(&r.body).unwrap_or_else(|e| panic!("{e}: {}", r.text()))
}

impl Env {
    async fn op(&self, s: &Login, what: &str, body: serde_json::Value) -> Res {
        self.call(s, "POST", &format!("/.cc/{what}"), body.to_string())
            .await
    }

    fn medien(&self, rel: &str) -> std::path::PathBuf {
        self._tmp.path().join("medien").join(rel)
    }
}

#[tokio::test]
async fn rename_with_name_clashes() {
    let e = env();
    let s = e.session("Jana", "familie").await;
    let r = e
        .op(
            &s,
            "rename",
            serde_json::json!({"vol": "Medien", "path": "a.txt", "name": "b.txt"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(json(&r)["path"], "b.txt");
    assert!(e.medien("b.txt").is_file() && !e.medien("a.txt").exists());
    assert!(e.audited(&["rename"]).await.contains(&"rename".to_string()));
    assert!(
        e.state
            .db
            .recent_audit(3)
            .unwrap()
            .iter()
            .any(|ev| ev.action == "rename"
                && ev.target == "Medien/a.txt"
                && ev.detail == "Medien/b.txt")
    );

    std::fs::write(e.medien("c.txt"), b"other").unwrap();
    let body = |clash: &str| serde_json::json!({"vol": "Medien", "path": "b.txt", "name": "c.txt", "clash": clash});
    let r = e.op(&s, "rename", body("ask")).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    let c = json(&r);
    assert_eq!(c["clash"]["name"], "c.txt");
    assert_eq!(c["clash"]["can_replace"], true);
    assert!(
        c["clash"]["new_text"].as_str().unwrap().contains("10 B"),
        "{c}"
    );
    let r = e.op(&s, "rename", body("skip")).await;
    assert_eq!(json(&r)["skipped"], true);
    assert!(e.medien("b.txt").is_file());
    let r = e.op(&s, "rename", body("keep_both")).await;
    assert_eq!(json(&r)["name"], "c (1).txt");
    assert_eq!(json(&r)["renamed"], true);
    assert_eq!(std::fs::read(e.medien("c.txt")).unwrap(), b"other");

    // Replace: the file in the way goes to the trash.
    std::fs::write(e.medien("d.txt"), b"old d").unwrap();
    let r = e
        .op(
            &s,
            "rename",
            serde_json::json!({"vol": "Medien", "path": "c (1).txt", "name": "d.txt", "clash": "replace"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(std::fs::read(e.medien("d.txt")).unwrap(), b"0123456789");
    let trashed = e.state.db.trash_items().unwrap();
    assert!(trashed.iter().any(|t| t.path == "d.txt"), "{trashed:?}");

    // Same name, bad names, internal names, other volumes' paths.
    for (body, status) in [
        (
            serde_json::json!({"vol": "Medien", "path": "d.txt", "name": "d.txt"}),
            StatusCode::CONFLICT,
        ),
        (
            serde_json::json!({"vol": "Medien", "path": "d.txt", "name": "x/y"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            serde_json::json!({"vol": "Medien", "path": "d.txt", "name": ".."}),
            StatusCode::BAD_REQUEST,
        ),
        (
            serde_json::json!({"vol": "Medien", "path": "../outside.txt", "name": "x"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            serde_json::json!({"vol": "Medien", "path": ".chestcraft-trash", "name": "x"}),
            StatusCode::NOT_FOUND,
        ),
        (
            serde_json::json!({"vol": "Medien", "path": "nope.txt", "name": "x"}),
            StatusCode::NOT_FOUND,
        ),
        (
            serde_json::json!({"vol": "Medien", "path": "d.txt", "name": "y", "clash": "maybe"}),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let r = e.op(&s, "rename", body.clone()).await;
        assert_eq!(r.status, status, "{body} → {}", r.text());
    }
    assert!(e.medien("d.txt").is_file());
}

#[tokio::test]
async fn rights_csrf_and_sign_in() {
    let e = env();
    let reader = e.session("Kim", "leser").await;
    let body = serde_json::json!({"vol": "Medien", "path": "a.txt", "name": "b.txt"});
    let r = e.op(&reader, "rename", body.clone()).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "r only");
    let r = e
        .op(&reader, "copy", serde_json::json!({"vol": "Medien", "path": "a.txt", "to_vol": "Medien", "to_dir": "Urlaub Gardasee"}))
        .await;
    assert_eq!(
        r.status,
        StatusCode::FORBIDDEN,
        "copy needs w at the target"
    );
    // Backups is invisible to Kim: same answer as a missing volume.
    let r = e
        .op(
            &reader,
            "rename",
            serde_json::json!({"vol": "Backups", "path": "dump.bin", "name": "x"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    let jana = e.session("Jana", "familie").await;
    // Without the CSRF header, and without a session.
    let r = e
        .send_body(
            "POST",
            "/.cc/rename",
            "127.0.0.1",
            &[JSON, ("cookie", &jana.cookie)],
            body.to_string(),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = e
        .send_body(
            "POST",
            "/.cc/rename",
            "127.0.0.1",
            &[JSON],
            body.to_string(),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // Replace needs the right to delete at the target: Eingang is "rw" for familie.
    std::fs::write(e._tmp.path().join("eingang/a.txt"), b"x").unwrap();
    let r = e
        .op(&jana, "copy", serde_json::json!({"vol": "Medien", "path": "a.txt", "to_vol": "Eingang", "to_dir": "", "clash": "replace"}))
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = e
        .op(&jana, "copy", serde_json::json!({"vol": "Medien", "path": "a.txt", "to_vol": "Eingang", "to_dir": ""}))
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(json(&r)["clash"]["can_replace"], false);
    assert!(e.medien("a.txt").is_file(), "nothing moved");
}

#[tokio::test]
async fn move_and_copy_between_folders_and_volumes() {
    let e = env();
    let s = e.session("Jana", "familie").await;
    let to = |what: &str, path: &str, vol: &str, dir: &str| {
        (
            what.to_string(),
            serde_json::json!({"vol": "Medien", "path": path, "to_vol": vol, "to_dir": dir}),
        )
    };
    let (w, b) = to("move", "a.txt", "Medien", "Urlaub Gardasee");
    let r = e.op(&s, &w, b).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(json(&r)["path"], "Urlaub Gardasee/a.txt");
    assert!(e.medien("Urlaub Gardasee/a.txt").is_file());

    // A folder can't go into itself.
    let (w, b) = to("move", "Urlaub Gardasee", "Medien", "Urlaub Gardasee");
    let r = e.op(&s, &w, b).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(json(&r)["error"], "inside");

    // Copy a folder into another volume; the original stays.
    let (w, b) = to("copy", "Urlaub Gardasee", "Eingang", "");
    let r = e.op(&s, &w, b).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let copied = e._tmp.path().join("eingang/Urlaub Gardasee");
    assert_eq!(
        std::fs::read(copied.join("IMG_2041.jpg")).unwrap(),
        b"jpegdata"
    );
    assert!(e.medien("Urlaub Gardasee/IMG_2041.jpg").is_file());
    // Again: keep both.
    let (w, mut b) = to("copy", "Urlaub Gardasee", "Eingang", "");
    b["clash"] = "keep_both".into();
    let r = e.op(&s, &w, b).await;
    assert_eq!(json(&r)["name"], "Urlaub Gardasee (1)");

    // Move into another volume: needs m here and w there.
    let (w, b) = to("move", "Urlaub Gardasee/a.txt", "Eingang", "");
    let r = e.op(&s, &w, b).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(e._tmp.path().join("eingang/a.txt").is_file());
    assert!(!e.medien("Urlaub Gardasee/a.txt").exists());
    // The target folder must exist and be visible.
    let (w, b) = to("copy", "page.html", "Eingang", "nirgends");
    assert_eq!(e.op(&s, &w, b).await.status, StatusCode::NOT_FOUND);
    let (w, b) = to("copy", "page.html", "Backups", "");
    assert_eq!(e.op(&s, &w, b).await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn shares_follow_a_rename() {
    let e = env();
    let s = e.session("Tom", "teiler").await;
    let (id, _) = e.create_share(&s, "Medien", "a.txt", &[]).await;
    let r = e
        .op(
            &s,
            "rename",
            serde_json::json!({"vol": "Medien", "path": "a.txt", "name": "notes.txt"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let share = e
        .state
        .db
        .shares()
        .unwrap()
        .into_iter()
        .find(|sh| sh.id == id)
        .unwrap();
    assert_eq!(share.path, "notes.txt");
}
