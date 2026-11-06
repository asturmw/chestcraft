//! Preferences, skins, backgrounds and the admin's appearance page.

use super::*;

/// The state of a fresh environment, before any request clones it.
fn state_mut(e: &mut Env) -> &mut AppState {
    Arc::get_mut(&mut e.state).expect("no request has run yet")
}

fn with_skins(e: &mut Env) {
    state_mut(e).skins = vec!["classic".into(), "sleek".into()];
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(w, h, image::Rgb([30, 60, 90])))
        .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

const BOUNDARY: &str = "----chestcraft-test-boundary";

impl Env {
    /// POST a multipart form: (name, file bytes or None for a text field, text).
    async fn multipart(&self, s: &Login, uri: &str, fields: &[(&str, &[u8], bool)]) -> Res {
        let mut body = Vec::new();
        for (name, data, is_file) in fields {
            body.extend(format!("--{BOUNDARY}\r\n").as_bytes());
            if *is_file {
                body.extend(
                    format!(
                        "Content-Disposition: form-data; name=\"{name}\"; filename=\"{name}.png\"\r\nContent-Type: image/png\r\n\r\n"
                    )
                    .as_bytes(),
                );
            } else {
                body.extend(
                    format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
                );
            }
            body.extend(*data);
            body.extend(b"\r\n");
        }
        body.extend(format!("--{BOUNDARY}--\r\n").as_bytes());
        let mut req = Request::builder()
            .method("POST")
            .uri(uri)
            .header("cookie", &s.cookie)
            .header(
                "content-type",
                format!("multipart/form-data; boundary={BOUNDARY}"),
            )
            .body(Body::from(body))
            .unwrap();
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

    /// Upload one picture set for `target`; returns the response.
    async fn upload_set(&self, s: &Login, target: &str, pictures: &[(&str, &[u8])]) -> Res {
        let mut fields: Vec<(&str, &[u8], bool)> = vec![
            ("csrf", s.csrf.as_bytes(), false),
            ("target", target.as_bytes(), false),
        ];
        fields.extend(pictures.iter().map(|(slot, data)| (*slot, *data, true)));
        self.multipart(s, "/.cc/account/backgrounds/upload", &fields)
            .await
    }

    fn page_of(&self, s: &Login, uri: &str) -> impl std::future::Future<Output = String> {
        let (cookie, uri) = (s.cookie.clone(), uri.to_string());
        let st = self.state.clone();
        async move {
            let mut req = Request::builder()
                .uri(uri)
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap();
            req.extensions_mut().insert(ConnectInfo(SocketAddr::new(
                "127.0.0.1".parse().unwrap(),
                40000,
            )));
            let res = router(st).oneshot(req).await.unwrap();
            let body = axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .unwrap();
            String::from_utf8_lossy(&body).into_owned()
        }
    }
}

fn set_ids_of(e: &Env, owner: &str) -> Vec<String> {
    e.state
        .looks
        .sets_of(owner)
        .into_iter()
        .map(|s| s.id)
        .collect()
}

#[tokio::test]
async fn preferences_change_how_pages_render() {
    let e = env();
    let s = e.session("jana", "familie").await;
    let page = e.page_of(&s, "/.cc/account").await;
    assert!(
        page.contains("action=\"/.cc/account\"") && page.contains("name=\"rotation\""),
        "{page}"
    );
    // One skin (or none) means nothing to choose.
    assert!(!page.contains("name=\"skin\""), "{page}");

    let r = e
        .form_post(
            &s,
            "/.cc/account",
            &[
                ("lang", "de"),
                ("theme", "dark"),
                ("keys", "vim"),
                ("rotation", "load"),
            ],
        )
        .await;
    assert_eq!(
        (r.status, r.header("location")),
        (StatusCode::SEE_OTHER, "/.cc/account?saved=1")
    );
    let page = e.page_of(&s, "/").await;
    assert!(
        page.contains("lang=\"de\"")
            && page.contains("data-theme=\"dark\"")
            && page.contains("data-keys=\"vim\""),
        "{page}"
    );
    let saved = e.state.db.user("jana").unwrap().unwrap().settings;
    assert_eq!(saved.rotation.as_deref(), Some("load"));

    // A quick switch sends one field and returns to its page; the rest stays.
    let r = e
        .form_post(
            &s,
            "/.cc/account",
            &[("theme", "system"), ("next", "/Medien/")],
        )
        .await;
    assert_eq!(r.header("location"), "/Medien/");
    let page = e.page_of(&s, "/").await;
    assert!(
        !page.contains("data-theme=") && page.contains("data-keys=\"vim\""),
        "{page}"
    );
    let r = e
        .form_post(
            &s,
            "/.cc/account",
            &[("lang", ""), ("next", "//evil.example")],
        )
        .await;
    assert_eq!(r.header("location"), "/");
    assert!(e.page_of(&s, "/").await.contains("lang=\"en\""));

    for bad in [
        ("theme", "neon"),
        ("keys", "emacs"),
        ("skin", "nope"),
        ("lang", "xx"),
    ] {
        let r = e.form_post(&s, "/.cc/account", &[bad]).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad:?}");
    }
    let r = e
        .send_body(
            "POST",
            "/.cc/account",
            "127.0.0.1",
            &[FORM, ("cookie", &s.cookie)],
            form(&[("csrf", "wrong"), ("theme", "dark")]),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(
        e.get("/.cc/account").await.header("location"),
        "/.cc/login?next=%2F.cc%2Faccount"
    );
}

#[tokio::test]
async fn skins_come_from_the_account_then_the_admin_then_the_first() {
    let mut e = env();
    with_skins(&mut e);
    assert!(e.get("/").await.text().contains("data-skin=\"classic\""));
    let admin = e.session("Tibor", "admin").await;
    let jana = e.session("jana", "familie").await;
    let page = e.page_of(&admin, "/.cc/admin/appearance").await;
    assert!(page.contains("name=\"share_skin\""), "{page}");

    let r = e
        .form_post(
            &admin,
            "/.cc/admin/appearance",
            &[
                ("default_skin", "sleek"),
                ("share_skin", "classic"),
                ("rotation", "never"),
            ],
        )
        .await;
    assert_eq!(r.header("location"), "/.cc/admin/appearance?saved=1");
    assert!(e.page_of(&jana, "/").await.contains("data-skin=\"sleek\""));
    // Visitors without an account get the share skin.
    assert!(e.get("/").await.text().contains("data-skin=\"classic\""));
    assert!(
        e.get("/.cc/login")
            .await
            .text()
            .contains("data-skin=\"classic\"")
    );

    let page = e.page_of(&jana, "/.cc/account").await;
    assert!(page.contains("Default (Sleek)"), "{page}");
    e.form_post(&jana, "/.cc/account", &[("skin", "classic")])
        .await;
    assert!(
        e.page_of(&jana, "/")
            .await
            .contains("data-skin=\"classic\"")
    );
    // A skin this build no longer has falls back to the default.
    e.state
        .db
        .update_user("jana", |u| u.settings.skin = Some("gone".into()))
        .unwrap();
    let jana = Login {
        cookie: jana.cookie.clone(),
        csrf: jana.csrf.clone(),
    };
    assert!(e.page_of(&jana, "/").await.contains("data-skin=\"sleek\""));

    // Only admins reach the appearance page, with valid values.
    assert_eq!(
        e.form_post(
            &jana,
            "/.cc/admin/appearance",
            &[("default_skin", "classic")]
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        e.form_post(&admin, "/.cc/admin/appearance", &[("rotation", "hourly")])
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    assert!(
        e.audited(&["appearance_changed"])
            .await
            .contains(&"appearance_changed".into())
    );
}

#[tokio::test]
async fn own_pictures_upload_show_rotate_and_delete() {
    let e = env();
    let s = e.session("jana", "familie").await;
    let other = e.session("mika", "familie").await;
    let r = e
        .upload_set(&s, "user:default", &[("wide", &png(320, 180))])
        .await;
    assert_eq!(
        (r.status, r.header("location")),
        (
            StatusCode::SEE_OTHER,
            "/.cc/account/backgrounds?skin=default&saved=1"
        ),
        "{}",
        r.text()
    );
    let ids = set_ids_of(&e, "jana");
    assert_eq!(ids.len(), 1);
    let id = &ids[0];
    let wide = format!("/.cc/bg/{id}/wide.jpg");

    // The new set is in use at once; every screen shape falls back to the landscape picture.
    let page = e.page_of(&s, "/").await;
    assert!(
        page.contains("class=\"backdrop\" data-state=\"image\" data-dim=\"2\""),
        "{page}"
    );
    assert_eq!(page.matches(&wide).count(), 3, "{page}");

    let r = e
        .send("GET", &wide, "127.0.0.1", &[("cookie", &s.cookie)])
        .await;
    assert_eq!(
        (r.status, r.header("content-type")),
        (StatusCode::OK, "image/jpeg")
    );
    assert!(r.header("cache-control").starts_with("private"));
    let small = format!("/.cc/bg/{id}/wide.small.jpg");
    assert_eq!(
        e.send("GET", &small, "127.0.0.1", &[("cookie", &s.cookie)])
            .await
            .status,
        StatusCode::OK
    );
    // Not for others, nor for missing slots.
    assert_eq!(e.get(&wide).await.status, StatusCode::NOT_FOUND);
    assert_eq!(
        e.send("GET", &wide, "127.0.0.1", &[("cookie", &other.cookie)])
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let tall = format!("/.cc/bg/{id}/tall.jpg");
    assert_eq!(
        e.send("GET", &tall, "127.0.0.1", &[("cookie", &s.cookie)])
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    // A second set with all three shapes; both rotate, "never" shows the first.
    let r = e
        .upload_set(
            &s,
            "user:default",
            &[
                ("wide", &png(160, 90)),
                ("square", &png(90, 120)),
                ("tall", &png(100, 160)),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    let ids = set_ids_of(&e, "jana");
    assert_eq!(ids.len(), 2);
    let bg = e
        .state
        .db
        .user("jana")
        .unwrap()
        .unwrap()
        .settings
        .backgrounds["default"]
        .clone();
    assert_eq!(crate::appearance::set_ids(&bg), ids.as_slice());
    e.form_post(&s, "/.cc/account", &[("rotation", "never")])
        .await;
    for _ in 0..3 {
        assert!(e.page_of(&s, "/").await.contains(&wide));
    }
    let editor = e.page_of(&s, "/.cc/account/backgrounds").await;
    assert!(
        editor.contains("2 of 10 picture sets")
            && editor.contains(&format!("value=\"{}\"", ids[1])),
        "{editor}"
    );

    // Save with only the second set ticked, darker and blurred.
    let r = e
        .form_post(
            &s,
            "/.cc/account/backgrounds",
            &[
                ("target", "user:default"),
                ("fill", "images"),
                ("sets", &ids[1]),
                ("sets", "someone-elses"),
                ("dim", "4"),
                ("blur", "1"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    let page = e.page_of(&s, "/").await;
    assert!(
        page.contains(&format!("/.cc/bg/{}/tall.jpg", ids[1]))
            && page.contains("data-dim=\"4\" data-blur=\"1\""),
        "{page}"
    );
    // No set ticked → asked again.
    let r = e
        .form_post(
            &s,
            "/.cc/account/backgrounds",
            &[("target", "user:default"), ("fill", "images"), ("dim", "0")],
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    // Someone else's set can't be deleted.
    let r = e
        .form_post(
            &other,
            "/.cc/account/backgrounds",
            &[("target", "user:default"), ("delete", &ids[1])],
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    // Deleting removes the files and the choice; the page falls back to no background.
    let r = e
        .form_post(
            &s,
            "/.cc/account/backgrounds",
            &[("target", "user:default"), ("delete", &ids[1])],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert!(!e.state.looks.dir().join(&ids[1]).exists());
    let settings = e.state.db.user("jana").unwrap().unwrap().settings;
    assert!(settings.backgrounds.is_empty(), "{settings:?}");
    assert!(
        e.page_of(&s, "/")
            .await
            .contains("class=\"backdrop\" data-state=\"none\"")
    );
    let actions = e.audited(&["background_added", "background_deleted"]).await;
    assert!(
        actions.iter().any(|a| a == "background_deleted"),
        "{actions:?}"
    );
}

#[tokio::test]
async fn colors_come_as_a_stylesheet() {
    let e = env();
    let s = e.session("jana", "familie").await;
    let r = e
        .form_post(
            &s,
            "/.cc/account/backgrounds",
            &[
                ("target", "user:default"),
                ("fill", "color"),
                ("stops", "2"),
                ("c1", "#1A2B3C"),
                ("c2", "#445566"),
                ("c3", "#ffffff"),
                ("angle", "90"),
                ("dim", "0"),
                ("blur", "0"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    let page = e.page_of(&s, "/").await;
    let href = "/.cc/backdrop.css?c=1a2b3c-445566&amp;a=90";
    assert!(
        page.contains(href) && page.contains("data-state=\"color\""),
        "{page}"
    );
    let r = e.get("/.cc/backdrop.css?c=1a2b3c-445566&a=90").await;
    assert_eq!(r.header("content-type"), "text/css; charset=utf-8");
    assert!(
        r.text()
            .contains("linear-gradient(90deg, #1a2b3c, #445566)"),
        "{}",
        r.text()
    );
    for bad in ["c=red", "c=1a2b3c}&a=0", "c=1a2b3c&a=x", ""] {
        let r = e.get(&format!("/.cc/backdrop.css?{bad}")).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{bad}");
    }
    for bad in [
        vec![("fill", "color"), ("stops", "4"), ("c1", "#000000")],
        vec![("fill", "color"), ("stops", "1"), ("c1", "black")],
        vec![
            ("fill", "color"),
            ("stops", "1"),
            ("c1", "#000000"),
            ("angle", "400"),
        ],
        vec![("fill", "none"), ("dim", "5")],
        vec![("fill", "plaid")],
    ] {
        let mut pairs = vec![("target", "user:default")];
        pairs.extend(bad.iter().copied());
        let r = e.form_post(&s, "/.cc/account/backgrounds", &pairs).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad:?}");
    }
    // "none" overrides a default; "default" removes the own choice again.
    e.form_post(
        &s,
        "/.cc/account/backgrounds",
        &[("target", "user:default"), ("fill", "none")],
    )
    .await;
    assert!(e.page_of(&s, "/").await.contains("data-state=\"none\""));
    e.form_post(
        &s,
        "/.cc/account/backgrounds",
        &[("target", "user:default"), ("fill", "default")],
    )
    .await;
    assert!(
        e.state
            .db
            .user("jana")
            .unwrap()
            .unwrap()
            .settings
            .backgrounds
            .is_empty()
    );
}

#[tokio::test]
async fn admin_defaults_and_share_background_are_visible_to_everyone() {
    let mut e = env();
    with_skins(&mut e);
    let admin = e.session("Tibor", "admin").await;
    let jana = e.session("jana", "familie").await;

    // Only admins change defaults.
    for target in ["default:classic", "share"] {
        let r = e.upload_set(&jana, target, &[("wide", &png(16, 9))]).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{target}");
    }
    assert!(set_ids_of(&e, "jana").is_empty());
    assert!(
        !e.page_of(&jana, "/.cc/admin/appearance?edit=share")
            .await
            .contains("bg-editor")
    );

    let r = e
        .upload_set(&admin, "share", &[("tall", &png(100, 160))])
        .await;
    assert_eq!(
        r.header("location"),
        "/.cc/admin/appearance?edit=share&saved=1",
        "{}",
        r.text()
    );
    let share_set = set_ids_of(&e, "tibor")[0].clone();
    let tall = format!("/.cc/bg/{share_set}/tall.jpg");
    let page = e.get("/").await.text();
    assert!(page.contains(&tall), "{page}");
    assert_eq!(e.get(&tall).await.status, StatusCode::OK);
    // Share pages show it too, before the password.
    let page = e.get("/s/doesnotexist1").await.text();
    assert!(page.contains(&tall), "{page}");

    let r = e
        .upload_set(&admin, "default:classic", &[("wide", &png(160, 90))])
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    let default_set = set_ids_of(&e, "tibor")
        .into_iter()
        .find(|id| *id != share_set)
        .unwrap();
    let wide = format!("/.cc/bg/{default_set}/wide.jpg");
    // An account without its own choice sees the default of its skin, and may load it.
    assert!(e.page_of(&jana, "/").await.contains(&wide));
    assert_eq!(
        e.send("GET", &wide, "127.0.0.1", &[("cookie", &jana.cookie)])
            .await
            .status,
        StatusCode::OK
    );
    // But can't put the admin's set into its own background.
    let r = e
        .form_post(
            &jana,
            "/.cc/account/backgrounds",
            &[
                ("target", "user:classic"),
                ("fill", "images"),
                ("sets", &default_set),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    // Not shown for the other skin.
    e.form_post(&jana, "/.cc/account", &[("skin", "sleek")])
        .await;
    assert!(!e.page_of(&jana, "/").await.contains(&wide));

    let page = e.page_of(&admin, "/.cc/admin/appearance").await;
    assert!(
        page.contains("/.cc/admin/appearance?edit=default:classic")
            && page.contains("1 picture set"),
        "{page}"
    );
    // Deleting the share set clears the share background.
    e.form_post(
        &admin,
        "/.cc/account/backgrounds",
        &[("target", "share"), ("delete", &share_set)],
    )
    .await;
    assert!(e.state.looks.appearance().share_background.is_none());
    assert_eq!(e.get(&tall).await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn upload_rights_limits_and_bad_files() {
    let mut e = env();
    {
        let cfg = Arc::get_mut(&mut state_mut(&mut e).cfg).unwrap();
        cfg.appearance.max_backgrounds_per_user = 1;
        cfg.appearance.deny_upload_roles = ["leser".to_string()].into();
    }
    let reader = e.session("lena", "leser").await;
    let r = e
        .upload_set(&reader, "user:default", &[("wide", &png(16, 9))])
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(r.text().contains("can't upload pictures"), "{}", r.text());
    let editor = e.page_of(&reader, "/.cc/account/backgrounds").await;
    assert!(
        editor.contains("class=\"bg-upload\" data-state=\"denied\"")
            && !editor.contains("type=\"file\""),
        "{editor}"
    );

    let s = e.session("jana", "familie").await;
    let r = e
        .upload_set(&s, "user:default", &[("wide", b"not a picture")])
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        r.text()
            .contains("Landscape (16:9): chestcraft can&#39;t read")
            || r.text().contains("Landscape (16:9): chestcraft can't read"),
        "{}",
        r.text()
    );
    let r = e.upload_set(&s, "user:default", &[("wide", b"")]).await;
    assert!(
        r.text().contains("Choose at least one picture"),
        "{}",
        r.text()
    );
    let too_big = vec![0u8; crate::appearance::MAX_UPLOAD_BYTES + 1];
    let r = e
        .upload_set(&s, "user:default", &[("tall", &too_big)])
        .await;
    assert!(r.text().contains("larger than 30 MB"), "{}", r.text());
    assert!(set_ids_of(&e, "jana").is_empty());

    let r = e
        .upload_set(&s, "user:default", &[("wide", &png(16, 9))])
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let r = e
        .upload_set(&s, "user:default", &[("wide", &png(16, 9))])
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(r.text().contains("the most you can keep"), "{}", r.text());
    assert_eq!(set_ids_of(&e, "jana").len(), 1);

    // Wrong token or a foreign origin: refused before anything is stored.
    let forged = Login {
        cookie: s.cookie.clone(),
        csrf: "x".repeat(32),
    };
    let r = e
        .upload_set(&forged, "user:default", &[("wide", &png(16, 9))])
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn leftovers_of_a_crash_are_swept() {
    let e = env();
    let s = e.session("jana", "familie").await;
    e.upload_set(&s, "user:default", &[("wide", &png(16, 9))])
        .await;
    let id = set_ids_of(&e, "jana")[0].clone();
    let dir = e.state.looks.dir().to_path_buf();
    std::fs::create_dir_all(dir.join(".tmp-half")).unwrap();
    std::fs::create_dir_all(dir.join("orphan")).unwrap();
    super::jobs::sweep_leftovers(e.state.clone()).await;
    let left: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|d| d.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left, std::slice::from_ref(&id));
    // A record whose files are gone is dropped, with its use in the settings.
    std::fs::remove_dir_all(dir.join(&id)).unwrap();
    super::jobs::sweep_leftovers(e.state.clone()).await;
    assert!(e.state.looks.set(&id).is_none());
    assert!(
        e.state
            .db
            .user("jana")
            .unwrap()
            .unwrap()
            .settings
            .backgrounds
            .is_empty()
    );
}

#[tokio::test]
async fn a_forced_password_change_still_gets_its_backdrop() {
    let e = env();
    e.add_user("neu", "familie", PW, true);
    let cookie = cookie_of(&e.login("neu", PW).await);
    let r = e
        .get_h("/.cc/backdrop.css?c=000000&a=0", &[("cookie", &cookie)])
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let r = e
        .get_h("/.cc/bg/none/wide.jpg", &[("cookie", &cookie)])
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = e.get_h("/.cc/account", &[("cookie", &cookie)]).await;
    assert_eq!(r.header("location"), "/.cc/password");
}
