//! The admin pages' navigation, tables, create forms and the "shown once" box (review Q14,
//! Q26, Q27).

use super::*;

impl Env {
    async fn text_of(&self, s: &Login, uri: &str) -> String {
        self.get_h(uri, &[("cookie", &s.cookie)]).await.text()
    }
}

/// The part of `page` from `start` to the next `end`.
fn part<'a>(page: &'a str, start: &str, end: &str) -> &'a str {
    let i = page
        .find(start)
        .unwrap_or_else(|| panic!("no {start}: {page}"));
    let rest = &page[i..];
    &rest[..rest.find(end).unwrap_or(rest.len())]
}

#[tokio::test]
async fn admin_pages_have_the_admin_navigation() {
    let e = env();
    let admin = e.session("Admin", "admin").await;
    let pages = [
        ("/.cc/admin/dashboard", "/.cc/admin/dashboard"),
        ("/.cc/admin/alerts", "/.cc/admin/alerts"),
        ("/.cc/admin/users", "/.cc/admin/users"),
        ("/.cc/admin/volumes", "/.cc/admin/volumes"),
        ("/.cc/shares?all=1", "/.cc/shares?all=1"),
        ("/.cc/admin/appearance", "/.cc/admin/appearance"),
        ("/.cc/admin/audit", "/.cc/admin/audit"),
    ];
    for (uri, current) in pages {
        let t = e.text_of(&admin, uri).await;
        let nav = part(&t, r#"<nav class="sidenav""#, "</nav>");
        assert!(nav.contains(r#"data-variant="admin""#), "{uri}: {nav}");
        // Every admin page, the current one marked, and the way back to the libraries.
        for (href, _) in pages {
            let href = href.replace('&', "&amp;");
            assert!(nav.contains(&format!(r#"href="{href}""#)), "{uri}: {href}");
        }
        assert_eq!(nav.matches(r#"aria-current="page""#).count(), 1, "{nav}");
        assert!(
            nav.contains(&format!(
                r#"<a class="tree__link" href="{current}" aria-current="page">"#
            )),
            "{uri}: {nav}"
        );
        assert!(
            nav.contains(r#"<a class="sidenav__link" href="/">"#),
            "{nav}"
        );
        // No library tree here; the drawer button says what it opens.
        assert!(!nav.contains(r#"href="/Medien/""#), "{nav}");
        assert!(t.contains(r#"aria-label="Open the admin menu""#), "{uri}");
    }
    // Normal pages keep the library tree, also for the admin.
    for uri in ["/Medien/", "/.cc/account", "/.cc/shares", "/.cc/trash"] {
        let t = e.text_of(&admin, uri).await;
        assert!(!t.contains(r#"data-variant="admin""#), "{uri}");
        assert!(t.contains(r#"href="/Medien/""#), "{uri}");
    }
    // Someone else asking for everyone's shares sees their own, with their own sidebar.
    let jana = e.session("jana", "familie").await;
    let t = e.text_of(&jana, "/.cc/shares?all=1").await;
    assert!(!t.contains(r#"data-variant="admin""#), "{t}");
}

#[tokio::test]
async fn account_menu_holds_account_admin_and_sign_out() {
    let e = env();
    let admin = e.session("Admin", "admin").await;
    let jana = e.session("jana", "familie").await;
    let menu_of = |t: &str| part(t, r#"id="account-menu""#, "</div>").to_string();
    let a = menu_of(&e.text_of(&admin, "/Medien/").await);
    assert_eq!(a.matches(r#"class="menu__item""#).count(), 3, "{a}");
    assert!(
        a.contains(r#"href="/.cc/account""#)
            && a.contains(r#"href="/.cc/admin/dashboard""#)
            && a.contains(r#"action="/.cc/logout""#),
        "{a}"
    );
    for gone in [
        "/.cc/admin/users",
        "/.cc/admin/volumes",
        "/.cc/trash",
        "/.cc/shares",
    ] {
        assert!(!a.contains(&format!(r#"href="{gone}""#)), "{gone}: {a}");
    }
    let j = menu_of(&e.text_of(&jana, "/Medien/").await);
    assert_eq!(j.matches(r#"class="menu__item""#).count(), 2, "{j}");
    assert!(!j.contains("/.cc/admin/"), "{j}");
}

#[tokio::test]
async fn accounts_page_tables_empty_states_and_row_actions() {
    let e = env();
    let admin = e.session("Admin", "admin").await;
    let _jana = e.session("jana", "familie").await;
    let t = e.text_of(&admin, "/.cc/admin/users").await;
    // Tables first, the create forms behind their buttons (closed), machines together.
    assert!(t.find(r#"class="admin-table users-table""#) < t.find(r#"id="machines""#));
    for id in ["add-person", "add-agent", "add-api-token"] {
        assert!(
            t.contains(&format!(
                r#"<details class="adder" id="{id}" data-js="adder" data-t-done"#
            )),
            "{id}: {t}"
        );
    }
    assert!(
        t.contains("No agents yet.") && t.contains("No tokens yet."),
        "{t}"
    );
    assert!(t.contains(r#"data-js="confirm-dialog""#));
    // Destructive row actions ask first (with script); unblocking doesn't.
    let row = part(
        &t,
        r#"<th scope="row"><span class="admin-table__name">jana"#,
        "</tr>",
    );
    assert!(
        row.contains(r#"data-confirm="New one-time password for jana?"#)
            && row.contains(r#"data-confirm="Block jana?"#),
        "{row}"
    );
    // One's own row: no role select, no block.
    let me = part(&t, r#"<span class="admin-table__name">Admin"#, "</tr>");
    assert!(
        !me.contains("<select") && !me.contains(r#"value="disable""#),
        "{me}"
    );

    // An agent with a token: the token in its row, with "+ Token" for more.
    e.form_post(
        &admin,
        "/.cc/admin/users",
        &[("op", "agent_add"), ("name", "bot"), ("role", "leser")],
    )
    .await;
    let r = e
        .form_post(
            &admin,
            "/.cc/admin/users",
            &[
                ("op", "agent_token"),
                ("name", "bot"),
                ("label", "laptop"),
                ("expires", "30"),
            ],
        )
        .await;
    assert_eq!(r.header("cache-control"), "no-store");
    let page = r.text();
    let result = part(&page, r#"data-js="secret-result""#, "</section>");
    assert!(
        result.contains(r#"<div class="secret-box">"#)
            && result.contains(r#"id="secret-0""#)
            && result.contains(r#"href="/.cc/admin/users#agents""#),
        "{result}"
    );
    let t = e.text_of(&admin, "/.cc/admin/users").await;
    let row = part(&t, r#"<span class="admin-table__name">bot"#, "</tr>");
    assert!(
        row.contains("<strong>laptop</strong>")
            && row.contains(r#"id="add-token-bot""#)
            && row.contains(r#"data-confirm="Revoke “laptop”?"#),
        "{row}"
    );
    assert!(!t.contains("No agents yet."));
}

#[tokio::test]
async fn create_forms_work_without_script() {
    let e = env();
    let admin = e.session("Admin", "admin").await;
    // New account: the page with the "shown once" box, never cached.
    let r = e
        .form_post(
            &admin,
            "/.cc/admin/users",
            &[("op", "add"), ("name", "Mia"), ("role", "leser")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.header("cache-control"), "no-store");
    let t = r.text();
    let result = part(&t, r#"data-js="secret-result""#, "</section>");
    assert!(
        result.contains(
            r#"<h1 class="secret-result__title" id="secret-title">Account Mia created</h1>"#
        ) && result.contains(r#"<div class="secret-box">"#)
            && result.contains(r#"id="secret-0" type="text" readonly value="Mia""#)
            && result.contains(r#"data-js="copy" data-copy="secret-1" hidden"#)
            && result.contains(r#"data-js="secret-back"><a href="/.cc/admin/users">"#),
        "{result}"
    );
    // Taken name: the page again, its form open with the reason, nothing else on top.
    let r = e
        .form_post(
            &admin,
            "/.cc/admin/users",
            &[("op", "add"), ("name", "mia"), ("role", "leser")],
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let t = r.text();
    let form = part(
        &t,
        r#"<details class="adder" id="add-person""#,
        "</details>",
    );
    assert!(
        form.contains(r#"data-js="adder" open"#) && form.contains("exists already"),
        "{form}"
    );
    assert!(
        !t.contains(r#"<p class="banner" data-state="error" role="status""#),
        "{t}"
    );
    // An error that isn't a create's goes on top.
    let r = e
        .form_post(
            &admin,
            "/.cc/admin/users",
            &[("op", "disable"), ("name", "Admin")],
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        r.text()
            .contains(r#"data-state="error" role="status" data-js="form-error""#)
    );

    // New one-time password: the same box.
    let r = e
        .form_post(
            &admin,
            "/.cc/admin/users",
            &[("op", "passwd"), ("name", "Mia")],
        )
        .await;
    assert_eq!(r.header("cache-control"), "no-store");
    assert!(r.text().contains(r#"<div class="secret-box">"#));

    // Public uploads: the box with link and password, never cached; a refused one opens
    // that volume's form again.
    let r = e
        .form_post(
            &admin,
            "/.cc/admin/volumes",
            &[("op", "open"), ("vol", "Share"), ("expiry", "1d")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.header("cache-control"), "no-store");
    let t = r.text();
    assert!(
        t.contains(r#"<div class="secret-box">"#)
            && t.contains(r#"id="secret-link""#)
            && t.contains(r#"id="secret-password""#)
            && t.contains("open the uploads again"),
        "{t}"
    );
    let r = e
        .form_post(
            &admin,
            "/.cc/admin/volumes",
            &[
                ("op", "open"),
                ("vol", "Share"),
                ("expiry", "1d"),
                ("own_password", "short"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let form = part(
        &r.text(),
        r#"<details class="adder" id="open-Share""#,
        "</details>",
    )
    .to_string();
    assert!(form.contains(r#"data-js="adder" open"#), "{form}");
}

#[tokio::test]
async fn volumes_page_holds_libraries_and_public_uploads() {
    let e = env();
    let admin = e.session("Admin", "admin").await;
    let t = e.text_of(&admin, "/.cc/admin/volumes").await;
    let row = |name: &str| {
        part(
            &t,
            &format!(r#"<a class="admin-table__name" href="/{name}/">"#),
            "</tr>",
        )
        .to_string()
    };
    // The path muted under the name, the settings as tags.
    let share = row("Share");
    assert!(
        share.contains(r#"<span class="admin-path">"#)
            && share
                .contains(r#"<span class="tag" title="Public (anyone can read)">public</span>"#)
            && share.contains(r#"id="open-Share""#),
        "{share}"
    );
    let backups = row("Backups");
    assert!(
        backups.contains(r#"title="Only reachable at home">home only</span>"#)
            && !backups.contains("adder"),
        "{backups}"
    );
    // Open uploads can be closed (asking first).
    e.form_post(
        &admin,
        "/.cc/admin/volumes",
        &[("op", "open"), ("vol", "Share"), ("expiry", "1d")],
    )
    .await;
    let t = e.text_of(&admin, "/.cc/admin/volumes").await;
    let share = part(&t, r#"href="/Share/""#, "</tr>");
    assert!(
        share.contains(r#"data-confirm="Close uploads to Share?"#)
            && share.contains(r#"value="close""#),
        "{share}"
    );
}

#[tokio::test]
async fn app_passwords_form_behind_its_button() {
    let e = env();
    let s = e.session("jana", "familie").await;
    let t = e.text_of(&s, "/.cc/account").await;
    assert!(
        t.contains(r#"<details class="adder" id="add-app-password" data-js="adder" data-t-done"#)
            && t.contains("No app passwords yet."),
        "{t}"
    );
    let r = e
        .form_post(
            &s,
            "/.cc/account/app-passwords",
            &[("op", "add"), ("label", "laptop")],
        )
        .await;
    assert_eq!(r.header("cache-control"), "no-store");
    assert!(r.text().contains(r#"<div class="secret-box">"#));
    // A refused one: back on the account page with its form open and the reason inside.
    e.form_post(
        &s,
        "/.cc/account/app-passwords",
        &[("op", "add"), ("label", "laptop")],
    )
    .await;
    let t = e.text_of(&s, "/.cc/account?app=duplicate").await;
    let form = part(
        &t,
        r#"<details class="adder" id="add-app-password""#,
        "</details>",
    );
    assert!(
        form.contains(r#"data-js="adder" open"#) && form.contains("already an app password"),
        "{form}"
    );
    assert!(t.contains(r#"data-confirm="Revoke “laptop”?"#), "{t}");
}
