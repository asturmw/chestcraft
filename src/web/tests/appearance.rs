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
    // Dim was never chosen: frosted glass gets the light veil.
    let page = e.page_of(&s, "/").await;
    assert!(
        page.contains("class=\"backdrop\" data-state=\"image\" data-dim=\"1\""),
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

fn all_skins(e: &mut Env) {
    state_mut(e).skins = ["anime", "classic", "gaming", "modern", "sleek"]
        .map(String::from)
        .into();
}

#[tokio::test]
async fn modern_is_the_skin_out_of_the_box() {
    let mut e = env();
    all_skins(&mut e);
    assert!(e.get("/").await.text().contains("data-skin=\"modern\""));
    let jana = e.session("jana", "familie").await;
    let page = e.page_of(&jana, "/").await;
    assert!(page.contains("data-skin=\"modern\""), "{page}");
    let page = e.page_of(&jana, "/.cc/account").await;
    assert!(page.contains("Default (Modern)"), "{page}");
    e.form_post(&jana, "/.cc/account", &[("lang", "de")]).await;
    let page = e.page_of(&jana, "/.cc/account").await;
    assert!(
        page.contains("Standard (Modern)") && page.contains(">Elegant<"),
        "{page}"
    );
}

#[tokio::test]
async fn glass_and_opacity_per_skin_with_admin_defaults() {
    let mut e = env();
    with_skins(&mut e);
    let admin = e.session("Tibor", "admin").await;
    let jana = e.session("jana", "familie").await;
    // Out of the box: frosted, the skin's own opacity, the skin's own backdrop.
    let page = e.page_of(&jana, "/").await;
    assert!(
        page.contains("data-glass=\"frosted\"")
            && !page.contains("data-opacity")
            && page.contains("class=\"backdrop\" data-state=\"none\" data-dim=\"0\""),
        "{page}"
    );
    let editor = e
        .page_of(&jana, "/.cc/account/backgrounds?skin=classic")
        .await;
    assert!(
        editor.contains("<option value=\"\" selected>Default (Frosted)</option>")
            && editor.contains("Default (As the skin has it)")
            && editor.contains("hard to read over bright pictures"),
        "{editor}"
    );

    // The admin's default for classic: clear, opacity 4, a color without a chosen dim.
    let r = e
        .form_post(
            &admin,
            "/.cc/account/backgrounds",
            &[
                ("target", "default:classic"),
                ("fill", "color"),
                ("stops", "1"),
                ("c1", "#102030"),
                ("dim", ""),
                ("glass", "clear"),
                ("opacity", "4"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    let a = e.state.looks.appearance();
    assert_eq!(a.panels["classic"].glass, Some(crate::db::Glass::Clear));
    assert_eq!(a.backgrounds["classic"].dim, None);
    let page = e.page_of(&jana, "/").await;
    assert!(
        page.contains("data-glass=\"clear\" data-opacity=\"4\"")
            && page.contains("data-state=\"color\" data-dim=\"2\""),
        "{page}"
    );
    let editor = e
        .page_of(&jana, "/.cc/account/backgrounds?skin=classic")
        .await;
    assert!(
        editor.contains("Default (Clear)") && editor.contains("Default (70%)"),
        "{editor}"
    );
    // Not for the other skin, nor for visitors.
    e.form_post(&jana, "/.cc/account", &[("skin", "sleek")])
        .await;
    assert!(
        e.page_of(&jana, "/")
            .await
            .contains("data-glass=\"frosted\" data-keys")
    );
    e.form_post(&jana, "/.cc/account", &[("skin", "classic")])
        .await;
    assert!(e.get("/").await.text().contains("data-glass=\"frosted\""));

    // The account's own glass wins; its "use the default" background keeps the admin's
    // color, with the dim of frosted glass.
    let r = e
        .form_post(
            &jana,
            "/.cc/account/backgrounds",
            &[
                ("target", "user:classic"),
                ("fill", "default"),
                ("glass", "frosted"),
                ("opacity", ""),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    let page = e.page_of(&jana, "/").await;
    assert!(
        page.contains("data-glass=\"frosted\" data-opacity=\"4\"")
            && page.contains("data-state=\"color\" data-dim=\"1\""),
        "{page}"
    );
    // An explicit dim stays; resetting the glass removes the entry again.
    e.form_post(
        &jana,
        "/.cc/account/backgrounds",
        &[
            ("target", "user:classic"),
            ("fill", "none"),
            ("dim", "3"),
            ("glass", ""),
            ("opacity", "1"),
        ],
    )
    .await;
    let page = e.page_of(&jana, "/").await;
    assert!(
        page.contains("data-glass=\"clear\" data-opacity=\"1\"")
            && page.contains("data-state=\"none\" data-dim=\"3\""),
        "{page}"
    );
    e.form_post(
        &jana,
        "/.cc/account/backgrounds",
        &[("target", "user:classic"), ("fill", "default")],
    )
    .await;
    assert!(
        e.state
            .db
            .user("jana")
            .unwrap()
            .unwrap()
            .settings
            .panels
            .is_empty()
    );

    // Share pages: their own panels.
    e.form_post(
        &admin,
        "/.cc/account/backgrounds",
        &[
            ("target", "share"),
            ("fill", "none"),
            ("glass", "clear"),
            ("opacity", "2"),
        ],
    )
    .await;
    let page = e.get("/s/doesnotexist1").await.text();
    assert!(
        page.contains("data-glass=\"clear\" data-opacity=\"2\""),
        "{page}"
    );

    for bad in [
        ("glass", "matte"),
        ("opacity", "0"),
        ("opacity", "6"),
        ("dim", "x"),
    ] {
        let r = e
            .form_post(
                &jana,
                "/.cc/account/backgrounds",
                &[("target", "user:classic"), ("fill", "none"), bad],
            )
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad:?}");
    }
}

#[tokio::test]
async fn presets_become_an_ordinary_gradient() {
    let mut e = env();
    all_skins(&mut e);
    let jana = e.session("jana", "familie").await;
    let editor = e
        .page_of(&jana, "/.cc/account/backgrounds?skin=gaming")
        .await;
    let order: Vec<usize> = ["\"neon\"", "\"graphite\"", "\"sakura-dusk\"", "\"mint\""]
        .iter()
        .map(|id| editor.find(&format!("data-preset={id}")).expect(id))
        .collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{editor}");
    assert!(editor.contains("/.cc/presets.css?v="), "{editor}");
    let css = e.get("/.cc/presets.css").await;
    assert_eq!(css.header("content-type"), "text/css; charset=utf-8");
    assert!(
        css.text()
            .contains(".bg-preset[data-preset=\"neon\"] { --preset: linear-gradient(135deg, #070a14, #2a0b3d, #0f5f73); }"),
        "{}",
        css.text()
    );

    // A stored dim and the panels stay when a swatch is picked.
    e.form_post(
        &jana,
        "/.cc/account/backgrounds",
        &[
            ("target", "user:gaming"),
            ("fill", "none"),
            ("dim", "3"),
            ("glass", "clear"),
        ],
    )
    .await;
    let r = e
        .form_post(
            &jana,
            "/.cc/account/backgrounds",
            &[("target", "user:gaming"), ("preset", "sakura-dusk")],
        )
        .await;
    assert_eq!(
        r.header("location"),
        "/.cc/account/backgrounds?skin=gaming&saved=1"
    );
    let settings = e.state.db.user("jana").unwrap().unwrap().settings;
    assert_eq!(
        settings.backgrounds["gaming"],
        crate::db::Background {
            fill: crate::db::Fill::Color {
                colors: vec!["#3b3a7d".into(), "#ef9bb8".into(), "#ffcba0".into()],
                angle: 160
            },
            dim: Some(3),
            blur: 0
        }
    );
    assert_eq!(
        settings.panels["gaming"].glass,
        Some(crate::db::Glass::Clear)
    );
    e.form_post(&jana, "/.cc/account", &[("skin", "gaming")])
        .await;
    let page = e.page_of(&jana, "/").await;
    assert!(
        page.contains("/.cc/backdrop.css?c=3b3a7d-ef9bb8-ffcba0&amp;a=160"),
        "{page}"
    );
    // The editor shows it as a color, ready to change.
    let editor = e
        .page_of(&jana, "/.cc/account/backgrounds?skin=gaming")
        .await;
    assert!(
        editor.contains("value=\"color\" checked") && editor.contains("value=\"#ef9bb8\""),
        "{editor}"
    );
    let r = e
        .form_post(
            &jana,
            "/.cc/account/backgrounds",
            &[("target", "user:gaming"), ("preset", "plaid")],
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn built_in_sets_are_for_everyone_and_never_deleted() {
    let e = env();
    assert_eq!(
        e.state
            .builtin_sets
            .iter()
            .map(|b| (b.name, b.slots.len()))
            .collect::<Vec<_>>(),
        [("mountains", 3), ("sakura", 3), ("garden", 3)]
    );
    let jana = e.session("jana", "familie").await;
    let admin = e.session("Tibor", "admin").await;
    let editor = e.page_of(&jana, "/.cc/account/backgrounds").await;
    let small = crate::web::assets::url("backgrounds/mountains-wide.small.jpg");
    assert!(
        editor.contains("value=\"builtin:mountains\"")
            && editor.contains("Use Mountains")
            && editor.contains(&small)
            && !editor.contains("value=\"builtin:mountains\">Delete"),
        "{editor}"
    );

    // Mixed with an own set; "never" shows the first.
    e.upload_set(&jana, "user:default", &[("wide", &png(16, 9))])
        .await;
    let own = set_ids_of(&e, "jana")[0].clone();
    let r = e
        .form_post(
            &jana,
            "/.cc/account/backgrounds",
            &[
                ("target", "user:default"),
                ("fill", "images"),
                ("sets", "builtin:sakura"),
                ("sets", &own),
                ("sets", "builtin:nope"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    let bg = &e
        .state
        .db
        .user("jana")
        .unwrap()
        .unwrap()
        .settings
        .backgrounds["default"];
    assert_eq!(
        crate::appearance::set_ids(bg),
        ["builtin:sakura".to_string(), own.clone()]
    );
    e.form_post(&jana, "/.cc/account", &[("rotation", "never")])
        .await;
    let page = e.page_of(&jana, "/").await;
    let wide = crate::web::assets::url("backgrounds/sakura-wide.jpg");
    let tall = crate::web::assets::url("backgrounds/sakura-tall.jpg");
    assert!(page.contains(&wide) && page.contains(&tall), "{page}");
    let r = e.get(&wide).await;
    assert_eq!(
        (r.status, r.header("content-type")),
        (StatusCode::OK, "image/jpeg")
    );
    assert!(r.header("cache-control").contains("immutable"));

    // Not deletable, and a sweep leaves them alone.
    let r = e
        .form_post(
            &jana,
            "/.cc/account/backgrounds",
            &[("target", "user:default"), ("delete", "builtin:sakura")],
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    super::jobs::sweep_leftovers(e.state.clone()).await;
    let bg = &e
        .state
        .db
        .user("jana")
        .unwrap()
        .unwrap()
        .settings
        .backgrounds["default"];
    assert_eq!(crate::appearance::set_ids(bg).len(), 2);

    // The admin's share background may use one; visitors see it.
    e.form_post(
        &admin,
        "/.cc/account/backgrounds",
        &[
            ("target", "share"),
            ("fill", "images"),
            ("sets", "builtin:garden"),
        ],
    )
    .await;
    let page = e.get("/s/doesnotexist1").await.text();
    assert!(
        page.contains(&crate::web::assets::url("backgrounds/garden-square.jpg")),
        "{page}"
    );
    // Deleting an own set keeps the built-in one in the background.
    e.form_post(
        &jana,
        "/.cc/account/backgrounds",
        &[("target", "user:default"), ("delete", &own)],
    )
    .await;
    let bg = &e
        .state
        .db
        .user("jana")
        .unwrap()
        .unwrap()
        .settings
        .backgrounds["default"];
    assert_eq!(
        crate::appearance::set_ids(bg),
        ["builtin:sakura".to_string()]
    );
}

#[tokio::test]
async fn skin_pictures_follow_the_theme() {
    let mut e = env();
    {
        let st = state_mut(&mut e);
        st.skins = ["classic", "modern", "sleek", "gaming"]
            .map(String::from)
            .into();
        let pic = |skin: &str, light: bool| crate::appearance::SkinPicture {
            dark: format!("backgrounds/default-{skin}-dark.svg"),
            light: light.then(|| format!("backgrounds/default-{skin}-light.svg")),
        };
        st.skin_pictures = [
            ("sleek".to_string(), pic("sleek", true)),
            ("gaming".to_string(), pic("gaming", false)),
        ]
        .into();
    }
    let jana = e.session("jana", "familie").await;
    let (dark, light) = (
        "/.cc/assets/backgrounds/default-sleek-dark.svg",
        "/.cc/assets/backgrounds/default-sleek-light.svg",
    );
    // modern: its backdrop color from the skin's tokens.
    assert!(
        e.page_of(&jana, "/")
            .await
            .contains("data-state=\"none\" data-dim=\"0\"")
    );
    e.form_post(&jana, "/.cc/account", &[("skin", "sleek")])
        .await;
    // System theme: the browser picks; frosted glass dims the picture lightly.
    let page = e.page_of(&jana, "/").await;
    assert!(
        page.contains("data-state=\"image\" data-dim=\"1\"")
            && page.contains(&format!(
                "<source media=\"(prefers-color-scheme: light)\" srcset=\"{light}\"><img class=\"backdrop__img\" src=\"{dark}\""
            )),
        "{page}"
    );
    e.form_post(&jana, "/.cc/account", &[("theme", "light")])
        .await;
    let page = e.page_of(&jana, "/").await;
    assert!(
        page.contains(&format!("src=\"{light}\"")) && !page.contains(dark),
        "{page}"
    );
    e.form_post(&jana, "/.cc/account", &[("theme", "dark")])
        .await;
    let page = e.page_of(&jana, "/").await;
    assert!(
        page.contains(&format!("src=\"{dark}\"")) && !page.contains(light),
        "{page}"
    );
    // Without a light file the dark one serves both themes.
    e.form_post(
        &jana,
        "/.cc/account",
        &[("skin", "gaming"), ("theme", "light")],
    )
    .await;
    let page = e.page_of(&jana, "/").await;
    assert!(
        page.contains("src=\"/.cc/assets/backgrounds/default-gaming-dark.svg\"")
            && !page.contains("prefers-color-scheme"),
        "{page}"
    );
    // "The skin's own" with clear glass: the picture, dimmed more.
    e.form_post(
        &jana,
        "/.cc/account/backgrounds",
        &[
            ("target", "user:gaming"),
            ("fill", "none"),
            ("glass", "clear"),
        ],
    )
    .await;
    let page = e.page_of(&jana, "/").await;
    assert!(
        page.contains("data-state=\"image\" data-dim=\"2\"")
            && page.contains("default-gaming-dark.svg"),
        "{page}"
    );
}
