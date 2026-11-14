//! The share view for visitors (name once, one line, the note, "Download all", filter from
//! 20 entries, no sign-in link) and the list/grid choice of every listing (by content, the
//! address, the account's setting, a visitor's cookie).

use super::*;

impl Env {
    /// A share by `who` and a visitor's cookie for it.
    async fn shared(&self, who: &Login, path: &str, extra: &[(&str, &str)]) -> (String, String) {
        let (id, pw) = self.create_share(who, "Medien", path, extra).await;
        let cookie = self.visit(&id, &pw).await;
        (id, cookie)
    }

    async fn page(&self, uri: &str, cookie: &str) -> String {
        let r = self.get_h(uri, &[("cookie", cookie)]).await;
        assert_eq!(r.status, StatusCode::OK, "{uri}: {}", r.text());
        r.text()
    }

    fn layout_of(&self, user: &str) -> Option<String> {
        self.state.db.user(user).unwrap().unwrap().settings.layout
    }
}

fn view(page: &str) -> &'static str {
    match (
        page.contains(r#"data-view="grid""#),
        page.contains(r#"data-view="list""#),
    ) {
        (true, false) => "grid",
        (false, true) => "list",
        _ => panic!("no layout in {page}"),
    }
}

#[tokio::test]
async fn share_root_shows_the_name_once_and_the_download_on_top() {
    let e = env();
    e.put("medien/Urlaub Gardasee/Tag 1/Strand.txt", b"12345");
    let admin = e.session("Admin", "admin").await;
    let (id, cookie) = e
        .shared(&admin, "Urlaub Gardasee", &[("note", "Fotos vom See")])
        .await;
    let t = e.page(&format!("/s/{id}/"), &cookie).await;

    // The name is the page title, and only that: no share header, no crumb repeating it.
    assert_eq!(t.matches("<h1").count(), 1, "{t}");
    assert!(
        t.contains(r#"<h1 class="browser__title">Urlaub Gardasee</h1>"#),
        "{t}"
    );
    assert!(
        !t.contains("share-head") && !t.contains(r#"class="crumbs""#),
        "{t}"
    );
    // One line: who and until when. The creator's note stays, the "view only" banner went.
    assert!(
        t.contains(r#"<p class="share-line">Shared by Admin · available until "#),
        "{t}"
    );
    assert!(
        t.contains(r#"<p class="banner share-note" data-state="note">Fotos vom See</p>"#),
        "{t}"
    );
    assert!(!t.contains(r#"data-state="read-only""#), "{t}");
    // Brand and language switch, but no way to sign in.
    assert!(
        t.contains(r#"class="brand""#) && t.contains(r#"class="langswitch""#),
        "{t}"
    );
    assert!(!t.contains("/.cc/login"), "{t}");

    // The primary button: a plain link to the folder's zip, with the whole size (the
    // picture's 8 bytes and the 5 in the subfolder).
    let button = format!(r#"<a class="btn btn--primary share-dl__all" href="/s/{id}/?zip">"#);
    assert!(t.contains(&button), "{t}");
    assert!(
        t.contains(r#"Download all <span class="share-dl__size" data-js="dl-all-size" data-t="({size})">(13 B)</span>"#),
        "{t}"
    );
    let r = e
        .get_h(&format!("/s/{id}/?zip"), &[("cookie", &cookie)])
        .await;
    assert_eq!(
        zip_names(&r.body),
        ["IMG_2041.jpg", "Tag 1/", "Tag 1/Strand.txt"]
    );
    // The other ways sit in one menu next to it: no second copy in the title row, and no
    // action bar for someone who can only download.
    assert_eq!(t.matches(r#"popovertarget="dl-folder""#).count(), 1, "{t}");
    assert!(
        t.contains(
            r#"<div class="dl share-dl__more"><button class="btn dl__toggle share-dl__toggle""#
        ),
        "{t}"
    );
    assert!(
        !t.contains("dl-folder-m") && !t.contains(r#"class="actionbar""#),
        "{t}"
    );

    // In a folder of the share: its path in the crumbs, its own name as the title, and the
    // button downloads that folder. The note greets at the top only.
    let t = e.page(&format!("/s/{id}/Tag%201/"), &cookie).await;
    assert!(
        t.contains(&format!(
            r#"<a class="crumbs__link" href="/s/{id}/">Urlaub Gardasee</a>"#
        )) && t.contains(r#"aria-current="page">Tag 1</a>"#),
        "{t}"
    );
    assert!(
        t.contains(r#"<h1 class="browser__title">Tag 1</h1>"#),
        "{t}"
    );
    assert!(
        t.contains(&format!(
            r#"<a class="btn btn--primary share-dl__all" href="/s/{id}/Tag%201/?zip">"#
        )) && t.contains("Download folder <span")
            && t.contains(">(5 B)</span>"),
        "{t}"
    );
    assert!(
        t.contains(r#"class="share-line""#) && !t.contains("share-note"),
        "{t}"
    );
    assert!(!t.contains("/.cc/login"), "{t}");

    // A share that never expires names only the sharer.
    let (id, cookie) = e
        .shared(&admin, "Urlaub Gardasee", &[("expiry", "never")])
        .await;
    let t = e.page(&format!("/s/{id}/"), &cookie).await;
    assert!(
        t.contains(r#"<p class="share-line">Shared by Admin</p>"#) && !t.contains("share-note"),
        "{t}"
    );

    // Unavailable shares offer no sign-in either; the app's own pages still do.
    let r = e.get("/s/123456789abc").await;
    assert!(!r.text().contains("/.cc/login"), "{}", r.text());
    let t = e.get("/Share/").await.text();
    assert!(
        t.contains(r#"href="/.cc/login""#) && t.contains(r#"data-state="read-only""#),
        "{t}"
    );
}

#[tokio::test]
async fn share_download_button_needs_something_to_download() {
    let e = env();
    std::fs::create_dir_all(e.root().join("medien/Leer")).unwrap();
    let tom = e.session("tom", "teiler").await;

    // An empty folder: nothing to download, so no button and no bar.
    let (id, cookie) = e.shared(&tom, "Leer", &[]).await;
    let t = e.page(&format!("/s/{id}/"), &cookie).await;
    assert!(
        !t.contains("share-dl") && !t.contains(r#"class="actionbar""#),
        "{t}"
    );
    assert!(t.contains("browser__empty"), "{t}");

    // Visitors who may upload see the upload button: the page's primary one while the
    // folder is empty, a plain one next to "Download all" afterwards.
    let (id, cookie) = e.shared(&tom, "Leer", &[("mode", "view_upload")]).await;
    let t = e.page(&format!("/s/{id}/"), &cookie).await;
    assert!(
        t.contains(r#"<button class="btn btn--primary actionbar__upload""#)
            && !t.contains("share-dl"),
        "{t}"
    );
    e.put("medien/Leer/eins.txt", b"1");
    let t = e.page(&format!("/s/{id}/"), &cookie).await;
    assert!(
        t.contains(r#"<button class="btn actionbar__upload""#)
            && t.contains("share-dl__all")
            && !t.contains("actionbar__download"),
        "{t}"
    );
    // One primary button on the page itself (dialogs have their own).
    let pane = &t[..t.find("<table").unwrap()];
    assert_eq!(pane.matches("btn--primary").count(), 1, "{pane}");
}

#[tokio::test]
async fn share_filter_shows_from_twenty_entries() {
    let e = env();
    for i in 0..19 {
        e.put(&format!("medien/Viele/Datei {i:02}.txt"), b"x");
    }
    let tom = e.session("tom", "teiler").await;
    let (id, cookie) = e.shared(&tom, "Viele", &[]).await;
    let t = e.page(&format!("/s/{id}/"), &cookie).await;
    assert!(!t.contains(r#"data-js="filter""#), "{t}");
    assert!(
        t.contains(r#"<div class="browser__tools" data-state="view-only">"#),
        "{t}"
    );
    e.put("medien/Viele/Datei 19.txt", b"x");
    let t = e.page(&format!("/s/{id}/"), &cookie).await;
    assert!(
        t.contains(r#"data-js="filter""#) && t.contains(r#"<div class="browser__tools">"#),
        "{t}"
    );
    // The account's own browser filters any folder.
    let t = e.page("/Medien/Urlaub%20Gardasee/", &tom.cookie).await;
    assert!(t.contains(r#"data-js="filter""#), "{t}");
}

#[tokio::test]
async fn single_file_share_is_one_card_with_one_download() {
    let e = env();
    let admin = e.session("Admin", "admin").await;
    let (id, pw) = e
        .create_share(&admin, "Medien", "a.txt", &[("note", "Bitte lesen")])
        .await;
    let cookie = e.visit(&id, &pw).await;
    let t = e.page(&format!("/s/{id}"), &cookie).await;
    assert_eq!(t.matches("<h1").count(), 1, "{t}");
    assert!(
        t.contains(
            r#"<h1 class="share-head__name">a.txt</h1><p class="share-line">Shared by Admin · "#
        ),
        "{t}"
    );
    assert!(t.contains(r#"data-state="note">Bitte lesen</p>"#), "{t}");
    assert_eq!(t.matches("a.txt?dl").count(), 1, "{t}");
    assert!(
        t.contains(&format!(
            r#"<a class="btn btn--primary file-card__download" href="/s/{id}/a.txt?dl" download>"#
        )),
        "{t}"
    );
    assert!(
        !t.contains("/.cc/login") && t.contains(r#"class="langswitch""#),
        "{t}"
    );
}

#[tokio::test]
async fn folders_of_pictures_open_as_a_grid() {
    let e = env();
    for n in ["a.jpg", "b.PNG", "c.heic"] {
        e.put(&format!("medien/Drei/{n}"), b"x");
        e.put(&format!("medien/Vier/{n}"), b"x");
        e.put(&format!("medien/Gemischt/{n}"), b"x");
        e.put(&format!("medien/Meist/{n}"), b"x");
    }
    e.put("medien/Vier/d.mp4", b"x");
    e.put("medien/Gemischt/liste.txt", b"x");
    e.put("medien/Meist/d.mov", b"x");
    e.put("medien/Meist/liste.txt", b"x");
    let jana = e.session("jana", "familie").await;
    let c = jana.cookie.as_str();

    // At least four entries, at least 80 % of them pictures or videos.
    assert_eq!(view(&e.page("/Medien/Vier/", c).await), "grid");
    assert_eq!(view(&e.page("/Medien/Meist/", c).await), "grid", "4 of 5");
    assert_eq!(view(&e.page("/Medien/Drei/", c).await), "list", "too few");
    assert_eq!(view(&e.page("/Medien/Gemischt/", c).await), "list", "75 %");
    assert_eq!(view(&e.page("/Medien/", c).await), "list");
    // A subfolder is an entry that is no picture: 4 of 5 still do, 4 of 6 don't.
    std::fs::create_dir(e.root().join("medien/Vier/Roh")).unwrap();
    assert_eq!(view(&e.page("/Medien/Vier/", c).await), "grid");
    std::fs::create_dir(e.root().join("medien/Vier/Roh 2")).unwrap();
    assert_eq!(view(&e.page("/Medien/Vier/", c).await), "list");
    std::fs::remove_dir(e.root().join("medien/Vier/Roh 2")).unwrap();

    // The toggle's links always name a layout; other links only when the address does.
    let t = e.page("/Medien/Vier/", c).await;
    assert!(
        t.contains(r#"href="/Medien/Vier/?layout=list" aria-label="List">"#)
            && t.contains(
                r#"href="/Medien/Vier/?layout=grid" aria-label="Grid" aria-current="true">"#
            ),
        "{t}"
    );
    assert!(t.contains(r#"href="/Medien/Vier/?sort=size""#), "{t}");
    assert_eq!(e.layout_of("jana"), None);
}

#[tokio::test]
async fn an_accounts_layout_choice_is_remembered_until_reset() {
    let e = env();
    for n in ["a.jpg", "b.jpg", "c.jpg", "d.jpg"] {
        e.put(&format!("medien/Bilder/{n}"), b"x");
    }
    let jana = e.session("jana", "familie").await;
    let c = jana.cookie.as_str();
    assert_eq!(view(&e.page("/Medien/Bilder/", c).await), "grid");

    // The address wins, and the account keeps it (no cookie for accounts).
    let r = e
        .get_h("/Medien/Bilder/?layout=list&sort=size", &[("cookie", c)])
        .await;
    let t = r.text();
    assert_eq!(view(&t), "list");
    assert!(
        t.contains("?layout=list&amp;sort=modified"),
        "links keep what the address names: {t}"
    );
    assert!(!r.header("set-cookie").contains("cc_layout"));
    assert_eq!(e.layout_of("jana").as_deref(), Some("list"));
    // From then on it beats the automatic choice, in every folder.
    assert_eq!(view(&e.page("/Medien/Bilder/", c).await), "list");
    assert_eq!(
        view(&e.page("/Medien/Bilder/?layout=grid", c).await),
        "grid"
    );
    assert_eq!(e.layout_of("jana").as_deref(), Some("grid"));
    assert_eq!(view(&e.page("/Medien/", c).await), "grid");
    // An unknown value is no choice.
    assert_eq!(view(&e.page("/Medien/?layout=tiles", c).await), "grid");
    assert_eq!(e.layout_of("jana").as_deref(), Some("grid"));

    // The account page shows the choice and can hand it back to "automatic".
    let t = e.page("/.cc/account", c).await;
    assert!(
        t.contains(r#"<span class="field__label">Folder view</span><select class="field__input" name="layout">"#),
        "{t}"
    );
    let i = t.find(r#"name="layout""#).unwrap();
    let select = &t[i..i + t[i..].find("</select>").unwrap()];
    assert!(
        select.contains(r#"<option value="">Automatic (grid for pictures and videos)</option>"#)
            && select.contains(r#"<option value="list">List</option>"#)
            && select.contains(r#"<option value="grid" selected>Grid</option>"#),
        "{select}"
    );
    let r = e
        .form_post(&jana, "/.cc/account", &[("layout", "tiles")])
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = e.form_post(&jana, "/.cc/account", &[("layout", "")]).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    assert_eq!(e.layout_of("jana"), None);
    assert_eq!(view(&e.page("/Medien/", c).await), "list");
    assert_eq!(view(&e.page("/Medien/Bilder/", c).await), "grid");
    let t = e.page("/.cc/account", c).await;
    assert!(
        t.contains(
            r#"<option value="" selected>Automatic (grid for pictures and videos)</option>"#
        ),
        "{t}"
    );
    // Saving another preference leaves the layout alone.
    e.form_post(&jana, "/.cc/account", &[("layout", "list")])
        .await;
    e.form_post(&jana, "/.cc/account", &[("keys", "vim")]).await;
    assert_eq!(e.layout_of("jana").as_deref(), Some("list"));

    // Accounts stored before the setting existed load, and mean "automatic".
    let old: crate::db::UserSettings =
        serde_json::from_str(r#"{"lang":"de","theme":null,"skin":null,"keys":"vim"}"#).unwrap();
    assert_eq!(old.layout, None);
    assert!(
        !serde_json::to_string(&old).unwrap().contains("layout"),
        "unset stays out of the record"
    );
}

#[tokio::test]
async fn a_visitors_layout_choice_lives_in_a_cookie() {
    let e = env();
    for n in ["a.jpg", "b.jpg", "c.jpg", "d.jpg"] {
        e.put(&format!("share/Bilder/{n}"), b"x");
        e.put(&format!("medien/Bilder/Tag 1/{n}"), b"x");
    }
    e.put("medien/Bilder/liste.txt", b"x");

    // Anonymous on a public volume: by content, until the toggle is used.
    let r = e.get("/Share/Bilder/").await;
    assert_eq!(view(&r.text()), "grid");
    assert_eq!(r.header("set-cookie"), "");
    assert_eq!(view(&e.get("/Share/").await.text()), "list");
    let r = e.get("/Share/Bilder/?layout=list").await;
    assert_eq!(view(&r.text()), "list");
    assert_eq!(
        r.header("set-cookie"),
        "cc_layout=list; Path=/; SameSite=Lax; Max-Age=31536000"
    );
    let list = [("cookie", "cc_layout=list")];
    let r = e.get_h("/Share/Bilder/", &list).await;
    assert_eq!(view(&r.text()), "list", "the cookie beats the content");
    // Nothing new to remember; the address still wins over the cookie.
    let r = e.get_h("/Share/Bilder/?layout=list", &list).await;
    assert_eq!(r.header("set-cookie"), "");
    let r = e.get_h("/Share/Bilder/?layout=grid", &list).await;
    assert_eq!(view(&r.text()), "grid");
    assert!(
        r.header("set-cookie")
            .starts_with("cc_layout=grid; Path=/;")
    );
    let r = e.get_h("/Share/", &[("cookie", "cc_layout=grid")]).await;
    assert_eq!(view(&r.text()), "grid");
    // Behind HTTPS the cookie is Secure; a failed page sets none.
    let r = e
        .send(
            "GET",
            "/Share/Bilder/?layout=list",
            "192.0.2.10",
            &[
                ("cf-connecting-ip", "203.0.113.9"),
                ("x-forwarded-proto", "https"),
            ],
        )
        .await;
    assert!(
        r.header("set-cookie").ends_with("Max-Age=31536000; Secure"),
        "{}",
        r.header("set-cookie")
    );
    let r = e.get("/Share/Nichts/?layout=list").await;
    assert_eq!(
        (r.status, r.header("set-cookie")),
        (StatusCode::NOT_FOUND, "")
    );

    // Share folders: the same, with a cookie that also reaches `/s/<id>/…`.
    let tom = e.session("tom", "teiler").await;
    let (id, cookie) = e.shared(&tom, "Bilder", &[]).await;
    let with = |extra: &str| format!("{cookie}; {extra}");
    assert_eq!(
        view(&e.page(&format!("/s/{id}/"), &cookie).await),
        "list",
        "a folder and a text file"
    );
    let t = e.page(&format!("/s/{id}/Tag%201/"), &cookie).await;
    assert_eq!(view(&t), "grid");
    assert!(
        t.contains(&format!(r#"href="/s/{id}/Tag%201/?layout=list""#)),
        "{t}"
    );
    let r = e
        .get_h(
            &format!("/s/{id}/Tag%201/?layout=list"),
            &[("cookie", &cookie)],
        )
        .await;
    assert_eq!(view(&r.text()), "list");
    assert_eq!(
        r.header("set-cookie"),
        "cc_layout=list; Path=/; SameSite=Lax; Max-Age=31536000"
    );
    let t = e
        .page(&format!("/s/{id}/Tag%201/"), &with("cc_layout=list"))
        .await;
    assert_eq!(view(&t), "list");
    let t = e.page(&format!("/s/{id}/"), &with("cc_layout=grid")).await;
    assert_eq!(view(&t), "grid");
    // A share page never touches an account, even in a signed-in browser.
    let r = e
        .get_h(
            &format!("/s/{id}/?layout=grid"),
            &[("cookie", &format!("{cookie}; {}", tom.cookie))],
        )
        .await;
    assert!(r.header("set-cookie").starts_with("cc_layout=grid;"));
    assert_eq!(e.layout_of("tom"), None);
}
