//! Phone (review step 4): "Select" in the row menu (select mode), the View menu that holds
//! layout, sort, direction and the hidden files, and the tab bar without the trash, which
//! the account page links to instead.

use super::*;

/// The View menu's markup (its groups follow each other, so the first double close is the
/// last group's and the menu's).
fn view_menu(page: &str) -> &str {
    let start = page
        .find(r#"<div class="menu viewmenu" id="view-menu""#)
        .unwrap_or_else(|| panic!("no View menu in {page}"));
    let rest = &page[start..];
    &rest[..rest.find("</div></div>").unwrap()]
}

/// The opening tag of the View menu's item with this label.
fn item<'a>(menu: &'a str, label: &str) -> &'a str {
    let at = menu
        .find(&format!(r#"<span class="menu__label">{label}</span>"#))
        .unwrap_or_else(|| panic!("no item {label} in {menu}"));
    let open = menu[..at].rfind("<a ").unwrap();
    &menu[open..open + menu[open..].find('>').unwrap() + 1]
}

/// The status bar's markup.
fn status_bar(page: &str) -> &str {
    let start = page.find(r#"<footer class="status""#).unwrap();
    &page[start..start + page[start..].find("</footer>").unwrap()]
}

#[tokio::test]
async fn row_menu_offers_select_wherever_a_listing_selects() {
    let e = env();
    e.act_as(Some(admin()));
    let t = e.get("/Medien/").await.text();
    // browser.js builds the row menu; "Select" comes first, from this string.
    assert!(t.contains(r#"data-t-select="Select""#), "{t}");
    assert!(t.contains(r#"data-js="row-menu-open""#), "{t}");

    // Anonymous visitors of a public volume select too (zip of the selection).
    e.act_as(None);
    let t = e.get("/Share/").await.text();
    assert!(
        t.contains(r#"data-t-select="Select""#) && t.contains(r#"data-js="pick""#),
        "{t}"
    );
    // German.
    let t = e
        .get_h("/Share/", &[("accept-language", "de")])
        .await
        .text();
    assert!(t.contains(r#"data-t-select="Auswählen""#), "{t}");
}

#[tokio::test]
async fn view_menu_links_change_one_thing_and_keep_the_rest() {
    let e = env();
    e.act_as(Some(admin()));
    let t = e
        .get("/Medien/?layout=grid&sort=size&order=desc&page=1")
        .await
        .text();
    // A native popover: it opens without script.
    assert!(
        t.contains(r#"<button class="btn viewmenu__toggle" type="button" aria-haspopup="menu" popovertarget="view-menu" aria-label="View">"#),
        "{t}"
    );
    // The list/grid toggle stays (from 1024 px; below, the stylesheet hides it).
    assert!(t.contains(r#"<div class="seg browser__layout""#), "{t}");
    let m = view_menu(&t);

    // Layout: keeps sort and direction (and the page, like the toggle).
    assert_eq!(
        item(m, "List"),
        r#"<a class="menu__item viewmenu__item" role="menuitemradio" href="/Medien/?layout=list&amp;sort=size&amp;order=desc" aria-checked="false">"#
    );
    assert_eq!(
        item(m, "Grid"),
        r#"<a class="menu__item viewmenu__item" role="menuitemradio" href="/Medien/?layout=grid&amp;sort=size&amp;order=desc" aria-checked="true">"#
    );
    // Sort column: keeps the direction and the layout the address names; back to page 1.
    assert!(
        item(m, "Name")
            .contains(r#"href="/Medien/?layout=grid&amp;order=desc" aria-checked="false""#),
        "{m}"
    );
    assert!(
        item(m, "Size").contains(
            r#"href="/Medien/?layout=grid&amp;sort=size&amp;order=desc" aria-checked="true""#
        ),
        "{m}"
    );
    assert!(
        item(m, "Modified").contains(
            r#"href="/Medien/?layout=grid&amp;sort=modified&amp;order=desc" aria-checked="false""#
        ),
        "{m}"
    );
    // Direction: keeps the column.
    assert!(
        item(m, "Ascending")
            .contains(r#"href="/Medien/?layout=grid&amp;sort=size" aria-checked="false""#),
        "{m}"
    );
    assert!(
        item(m, "Descending").contains(
            r#"href="/Medien/?layout=grid&amp;sort=size&amp;order=desc" aria-checked="true""#
        ),
        "{m}"
    );
    // Hidden files (admin): a checkbox item that switches them on, keeping the rest.
    assert_eq!(
        item(m, "Show hidden files"),
        r#"<a class="menu__item viewmenu__item" role="menuitemcheckbox" href="/Medien/?layout=grid&amp;sort=size&amp;order=desc&amp;hidden=1" aria-checked="false">"#
    );
    // The status bar no longer carries it.
    let bar = status_bar(&t);
    assert!(
        !bar.contains("hidden=1") && !bar.contains("hidden files") && !bar.contains("<a "),
        "{bar}"
    );

    // Showing them: checked, and the link switches them off again.
    let t = e.get("/Medien/?hidden=1").await.text();
    let m = view_menu(&t);
    assert_eq!(
        item(m, "Show hidden files"),
        r#"<a class="menu__item viewmenu__item" role="menuitemcheckbox" href="/Medien/" aria-checked="true">"#
    );
    assert!(
        item(m, "Size").contains(r#"href="/Medien/?sort=size&amp;hidden=1""#),
        "{m}"
    );
    assert!(item(m, "List").contains(r#"href="/Medien/?layout=list&amp;hidden=1""#));
    assert!(!status_bar(&t).contains("hidden files"));
    // Without a layout in the address the links name none (memory or content decide).
    assert!(
        item(m, "Name").contains(r#"href="/Medien/?hidden=1" aria-checked="true""#),
        "{m}"
    );
}

#[tokio::test]
async fn view_menu_has_hidden_files_for_admins_only() {
    let e = env();
    e.act_as(Some(Principal::User {
        name: "jana".into(),
        role: "familie".into(),
    }));
    let t = e.get("/Medien/").await.text();
    let m = view_menu(&t);
    for label in [
        "List",
        "Grid",
        "Name",
        "Size",
        "Modified",
        "Ascending",
        "Descending",
    ] {
        item(m, label);
    }
    assert!(!m.contains(r#"data-group="hidden""#), "{m}");
    // hidden=1 does nothing for others, and no link offers it.
    let t = e.get("/Medien/?hidden=1").await.text();
    assert!(!t.contains("hidden=1"), "{t}");

    // Anonymous visitors of a public volume: the menu without hidden files.
    e.act_as(None);
    let t = e.get("/Share/").await.text();
    let m = view_menu(&t);
    item(m, "Size");
    assert!(!m.contains(r#"data-group="hidden""#), "{m}");
}

#[tokio::test]
async fn share_folders_get_the_view_menu_without_hidden_files() {
    let e = env();
    std::fs::write(e.root().join("medien/Urlaub Gardasee/.versteckt2"), b"x").unwrap();
    let admin = e.session("Admin", "admin").await;
    let (id, pw) = e
        .create_share(&admin, "Medien", "Urlaub Gardasee", &[])
        .await;
    let cookie = e.visit(&id, &pw).await;
    let t = e
        .get_h(&format!("/s/{id}/?sort=modified"), &[("cookie", &cookie)])
        .await
        .text();
    let m = view_menu(&t);
    assert!(
        item(m, "Descending").contains(&format!(r#"href="/s/{id}/?sort=modified&amp;order=desc""#)),
        "{m}"
    );
    assert!(!m.contains(r#"data-group="hidden""#), "{m}");
    assert!(t.contains(r#"data-t-select="Select""#), "{t}");
}

#[tokio::test]
async fn tab_bar_has_no_trash_and_the_account_page_links_to_it() {
    let e = env();
    let s = e.session("Admin", "admin").await;
    let t = e.get_h("/Medien/", &[("cookie", &s.cookie)]).await.text();
    let start = t.find(r#"<nav class="tabbar""#).unwrap();
    let bar = &t[start..start + t[start..].find("</nav>").unwrap()];
    assert_eq!(bar.matches(r#"class="tabbar__item""#).count(), 4, "{bar}");
    assert!(
        bar.contains(r#"href="/""#)
            && bar.contains(r#"href="/.cc/search?from=tab&amp;vol=Medien""#)
            && bar.contains(r#"href="/.cc/shares""#)
            && bar.contains(r#"href="/.cc/account""#)
            && !bar.contains("/.cc/trash"),
        "{bar}"
    );
    // The sidebar keeps its trash link; the account menu holds only the account, the
    // admin's way in and signing out (review Q14).
    assert!(
        t.contains(r#"<a class="sidenav__link" href="/.cc/trash""#),
        "{t}"
    );
    assert!(
        !t.contains(r#"<a class="menu__item" role="menuitem" href="/.cc/trash">"#),
        "{t}"
    );

    // The account page: the trash near the top.
    let t = e
        .get_h("/.cc/account", &[("cookie", &s.cookie)])
        .await
        .text();
    assert!(
        t.contains(r#"<p class="account__trash"><a href="/.cc/trash">"#),
        "{t}"
    );
    assert!(t.find("account__trash") < t.find("prefs-title"), "{t}");

    // On the trash page the account tab is the current one.
    let t = e.get_h("/.cc/trash", &[("cookie", &s.cookie)]).await.text();
    assert!(
        t.contains(r#"<a class="tabbar__item" href="/.cc/account" aria-current="page">"#),
        "{t}"
    );
}
