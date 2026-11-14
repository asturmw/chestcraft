//! The viewer page (`?view`, review Q7, Q18): no app shell, its own stylesheet and script,
//! previous/next and "3 / 18" in the listing's order (folders skipped, files without a
//! preview included), the close link back to the listing's page and row, the info panel,
//! actions by right, share visitors; and two share-view details (brand link, summary).

use super::*;

impl Env {
    async fn page_as(&self, uri: &str, cookie: &str) -> String {
        let r = self.get_h(uri, &[("cookie", cookie)]).await;
        assert_eq!(r.status, StatusCode::OK, "{uri}: {}", r.text());
        r.text()
    }

    /// `Reihe`: four files of different sizes (one without a preview) and two folders.
    fn row_folder(&self) {
        self.put("medien/Reihe/a.txt", b"aaa");
        self.put("medien/Reihe/b.txt", b"b");
        self.put("medien/Reihe/c.txt", b"cc");
        self.put("medien/Reihe/d.zip", b"dddd");
        self.put("medien/Reihe/0-ordner/x.txt", b"x");
        self.put("medien/Reihe/z-ordner/y.txt", b"y");
    }
}

/// The `href` of the element whose `data-js` is `js`, if there is one.
fn link(page: &str, js: &str) -> Option<String> {
    let at = page.find(&format!(r#"data-js="{js}""#))?;
    let start = page[..at].rfind('<')?;
    let tag = &page[start..page[start..].find('>')? + start];
    let h = tag.find(r#"href=""#)? + 6;
    Some(tag[h..h + tag[h..].find('"')?].replace("&amp;", "&"))
}

fn count(page: &str) -> Option<String> {
    let at = page.find(r#"<p class="viewer__count"><span aria-hidden="true">"#)?;
    let rest = &page[at + 50..];
    Some(rest[..rest.find('<')?].to_string())
}

#[tokio::test]
async fn the_viewer_is_a_page_of_its_own() {
    let e = env();
    e.put("medien/Urlaub Gardasee/IMG_2042.jpg", &jpeg(300, 200));
    let leser = e.session("Leser", "leser").await;
    let t = e
        .page_as("/Medien/Urlaub%20Gardasee/IMG_2042.jpg?view", &leser.cookie)
        .await;
    for shell in [
        r#"class="appbar""#,
        r#"class="sidenav""#,
        r#"class="tabbar""#,
        r#"class="backdrop""#,
        "/.cc/backdrop.css",
    ] {
        assert!(!t.contains(shell), "{shell}: {t}");
    }
    assert!(
        t.contains(&assets::url("viewer.css")) && t.contains(&assets::url("viewer.js")),
        "{t}"
    );
    // Dark in both themes, solid whatever the glass setting says.
    assert!(
        t.contains(r#"data-theme="dark""#) && t.contains(r#"data-glass="frosted""#),
        "{t}"
    );
    assert!(t.contains(r#"<body class="page page--viewer">"#), "{t}");
    assert!(
        t.contains(
            r#"<h1 class="viewer__name" id="viewer-name" title="IMG_2042.jpg">IMG_2042.jpg</h1>"#
        ),
        "{t}"
    );
    assert_eq!(count(&t).as_deref(), Some("2 / 2"), "{t}");
    // The original for 100 % and its size from the header, without decoding it.
    assert!(
        t.contains(
            r#"data-full="/Medien/Urlaub%20Gardasee/IMG_2042.jpg" data-w="300" data-h="200""#
        ),
        "{t}"
    );
    assert!(t.contains("300 × 200 px"), "{t}");
    // No other page's scripts: the listing's, the upload client.
    assert!(!t.contains("browser.js") && !t.contains("upload.js"), "{t}");
}

#[tokio::test]
async fn previous_and_next_follow_the_listing_order_and_skip_folders() {
    let e = env();
    e.row_folder();
    let leser = e.session("Leser", "leser").await;
    let c = leser.cookie.as_str();

    // By name: a, b, c, d.zip; folders don't count.
    let t = e.page_as("/Medien/Reihe/b.txt?view", c).await;
    assert_eq!(count(&t).as_deref(), Some("2 / 4"));
    assert_eq!(
        link(&t, "viewer-prev").as_deref(),
        Some("/Medien/Reihe/a.txt?view")
    );
    assert_eq!(
        link(&t, "viewer-next").as_deref(),
        Some("/Medien/Reihe/c.txt?view")
    );
    // Back to the listing at the row: two folders first, then a, b → row 3.
    assert_eq!(
        link(&t, "viewer-close").as_deref(),
        Some("/Medien/Reihe/#row-3")
    );
    let first = e.page_as("/Medien/Reihe/a.txt?view", c).await;
    assert!(link(&first, "viewer-prev").is_none(), "{first}");
    assert_eq!(count(&first).as_deref(), Some("1 / 4"));
    // The last one has no preview: it is in the sequence, with the download stage.
    let last = e.page_as("/Medien/Reihe/d.zip?view", c).await;
    assert!(link(&last, "viewer-next").is_none(), "{last}");
    assert_eq!(
        link(&last, "viewer-prev").as_deref(),
        Some("/Medien/Reihe/c.txt?view")
    );
    assert!(
        last.contains(r#"data-state="none""#) && last.contains("viewer__nothing-download"),
        "{last}"
    );

    // By size (b 1, c 2, a 3, d 4), as the listing sorted it: the order travels along.
    let t = e.page_as("/Medien/Reihe/a.txt?view&sort=size", c).await;
    assert_eq!(count(&t).as_deref(), Some("3 / 4"));
    assert_eq!(
        link(&t, "viewer-prev").as_deref(),
        Some("/Medien/Reihe/c.txt?view&sort=size")
    );
    assert_eq!(
        link(&t, "viewer-next").as_deref(),
        Some("/Medien/Reihe/d.zip?view&sort=size")
    );
    assert_eq!(
        link(&t, "viewer-close").as_deref(),
        Some("/Medien/Reihe/?sort=size#row-4")
    );
    // Descending: d, a, c, b.
    let t = e
        .page_as("/Medien/Reihe/a.txt?view&sort=size&order=desc", c)
        .await;
    assert_eq!(count(&t).as_deref(), Some("2 / 4"));
    assert_eq!(
        link(&t, "viewer-prev").as_deref(),
        Some("/Medien/Reihe/d.zip?view&sort=size&order=desc")
    );

    // The listing hands its order to the viewer and gives every row its id.
    let l = e.page_as("/Medien/Reihe/?sort=size", c).await;
    assert!(
        l.contains(r#"id="row-4""#) && l.contains(r#"href="a.txt?view&amp;sort=size""#),
        "{l}"
    );
}

#[tokio::test]
async fn the_close_link_finds_the_listing_page() {
    let e = env();
    // More files than one listing page holds: the last is on page 2.
    for i in 0..2001 {
        e.put(&format!("medien/Viele/f{i:04}.txt"), b"x");
    }
    let leser = e.session("Leser", "leser").await;
    let t = e
        .page_as("/Medien/Viele/f2000.txt?view", &leser.cookie)
        .await;
    assert_eq!(
        link(&t, "viewer-close").as_deref(),
        Some("/Medien/Viele/?page=2#row-0")
    );
    assert_eq!(count(&t).as_deref(), Some("2,001 / 2,001"));
}

#[tokio::test]
async fn the_info_panel_opens_by_link() {
    let e = env();
    e.row_folder();
    let leser = e.session("Leser", "leser").await;
    let t = e.page_as("/Medien/Reihe/b.txt?view", &leser.cookie).await;
    assert!(
        t.contains(r#"data-js="viewer-info-panel" hidden"#)
            && t.contains(r#"aria-expanded="false""#),
        "{t}"
    );
    assert_eq!(
        link(&t, "viewer-info").as_deref(),
        Some("/Medien/Reihe/b.txt?view&info=1")
    );
    let t = e
        .page_as("/Medien/Reihe/b.txt?view&info=1", &leser.cookie)
        .await;
    assert!(
        !t.contains(r#"data-js="viewer-info-panel" hidden"#)
            && t.contains(r#"aria-expanded="true""#),
        "{t}"
    );
    assert!(t.contains("<dt>Size</dt><dd>1 B</dd>"), "{t}");
    assert!(t.contains("<dd>Medien / Reihe</dd>"), "{t}");
    // Stepping keeps it open; the toggle closes it.
    assert_eq!(
        link(&t, "viewer-next").as_deref(),
        Some("/Medien/Reihe/c.txt?view&info=1")
    );
    assert_eq!(
        link(&t, "viewer-info").as_deref(),
        Some("/Medien/Reihe/b.txt?view")
    );
}

#[tokio::test]
async fn actions_follow_the_rights() {
    let e = env();
    let leser = e.session("Leser", "leser").await;
    let t = e.page_as("/Medien/a.txt?view", &leser.cookie).await;
    assert!(t.contains(r#"href="/Medien/a.txt?dl" download"#), "{t}");
    assert!(
        !t.contains("viewer__delete")
            && !t.contains("viewer-delete-dialog")
            && !t.contains("viewer__share"),
        "{t}"
    );

    let teiler = e.session("Teiler", "teiler").await;
    let t = e.page_as("/Medien/a.txt?view", &teiler.cookie).await;
    assert!(
        t.contains(r#"<form class="viewer__delete" method="post" action="/Medien/" data-js="viewer-delete">"#)
            && t.contains(r#"<input type="hidden" name="n" value="a.txt">"#),
        "{t}"
    );
    // The question the script asks first, with how long the trash keeps it.
    assert!(
        t.contains(r#"data-js="viewer-delete-dialog""#)
            && t.contains("Move “a.txt” to the trash?")
            && t.contains("Deleted items stay in the trash"),
        "{t}"
    );
    assert!(
        t.contains(r#"href="/.cc/shares/new?vol=Medien&amp;path=a.txt""#),
        "{t}"
    );
    // Icons carry their names.
    assert!(t.contains(r#"aria-label="Download""#), "{t}");
}

#[tokio::test]
async fn players_never_start_by_themselves() {
    let e = env();
    e.put("medien/Clips/clip.mp4", b"not really a video");
    e.put("medien/Clips/ton.mp3", b"not really audio");
    let leser = e.session("Leser", "leser").await;
    let t = e
        .page_as("/Medien/Clips/clip.mp4?view", &leser.cookie)
        .await;
    assert!(t.contains("<video") && t.contains(" controls"), "{t}");
    assert!(!t.contains("autoplay"), "{t}");
    let t = e.page_as("/Medien/Clips/ton.mp3?view", &leser.cookie).await;
    assert!(t.contains("<audio") && !t.contains("autoplay"), "{t}");
}

#[tokio::test]
async fn share_visitors_get_the_same_viewer_with_downloads_only() {
    let e = env();
    e.row_folder();
    let admin = e.session("Admin", "admin").await;
    let (id, pw) = e.create_share(&admin, "Medien", "Reihe", &[]).await;
    let cookie = e.visit(&id, &pw).await;
    let t = e.page_as(&format!("/s/{id}/b.txt?view"), &cookie).await;
    assert!(
        !t.contains(r#"class="appbar""#) && !t.contains("share-line"),
        "{t}"
    );
    assert!(!t.contains("/.cc/login"), "{t}");
    assert!(
        !t.contains("viewer__delete") && !t.contains("/.cc/shares/new"),
        "{t}"
    );
    assert!(
        t.contains(&format!(r#"href="/s/{id}/b.txt?dl" download"#)),
        "{t}"
    );
    assert_eq!(count(&t).as_deref(), Some("2 / 4"));
    assert_eq!(
        link(&t, "viewer-next").as_deref(),
        Some(format!("/s/{id}/c.txt?view").as_str())
    );
    assert_eq!(
        link(&t, "viewer-close").as_deref(),
        Some(format!("/s/{id}/#row-3").as_str())
    );
    // In a subfolder of the share, close returns there.
    let t = e
        .page_as(&format!("/s/{id}/0-ordner/x.txt?view"), &cookie)
        .await;
    assert_eq!(
        link(&t, "viewer-close").as_deref(),
        Some(format!("/s/{id}/0-ordner/#row-0").as_str())
    );
    assert!(t.contains("<dd>Reihe / 0-ordner</dd>"), "{t}");
}

#[tokio::test]
async fn the_brand_leads_to_the_share_not_the_sign_in() {
    let e = env();
    e.row_folder();
    let admin = e.session("Admin", "admin").await;
    let (id, pw) = e.create_share(&admin, "Medien", "Reihe", &[]).await;
    let cookie = e.visit(&id, &pw).await;
    for page in [format!("/s/{id}/"), format!("/s/{id}/0-ordner/")] {
        let t = e.page_as(&page, &cookie).await;
        assert!(
            t.contains(&format!(r#"<a class="brand" href="/s/{id}/">"#)),
            "{page}: {t}"
        );
        assert!(!t.contains(r#"<a class="brand" href="/">"#), "{t}");
    }
    // A single shared file: the page is the share; the brand is no link.
    let (id, pw) = e.create_share(&admin, "Medien", "Reihe/a.txt", &[]).await;
    let cookie = e.visit(&id, &pw).await;
    let t = e.page_as(&format!("/s/{id}"), &cookie).await;
    assert!(t.contains(r#"<span class="brand">"#), "{t}");
    assert!(!t.contains(r#"<a class="brand""#), "{t}");
    // The app's own pages keep the link home.
    let t = e.page_as("/Medien/", &admin.cookie).await;
    assert!(t.contains(r#"<a class="brand" href="/">"#), "{t}");
}

#[tokio::test]
async fn share_summaries_count_without_a_size() {
    let e = env();
    e.row_folder();
    let admin = e.session("Admin", "admin").await;
    let (id, pw) = e.create_share(&admin, "Medien", "Reihe", &[]).await;
    let cookie = e.visit(&id, &pw).await;
    let t = e.page_as(&format!("/s/{id}/"), &cookie).await;
    assert!(
        t.contains(r#"<p class="browser__summary">6 items</p>"#),
        "{t}"
    );
    // The button says how much it is (all of it, subfolders too).
    assert!(t.contains(">(12 B)</span>"), "{t}");
    // An account's own listing keeps the size of the files.
    let t = e.page_as("/Medien/Reihe/", &admin.cookie).await;
    assert!(
        t.contains(r#"<p class="browser__summary">6 items, 10 B</p>"#),
        "{t}"
    );
}
