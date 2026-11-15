//! Search (review step 5): `/.cc/search` for accounts, names only, every readable library
//! with the current one first; the one field for filter and search in the app bar (and its
//! phone copy in the title row); the Search tab.

use super::*;

impl Env {
    async fn search(&self, s: &Login, query: &str) -> String {
        let r = self
            .get_h(&format!("/.cc/search?{query}"), &[("cookie", &s.cookie)])
            .await;
        assert_eq!(r.status, StatusCode::OK, "{query}: {}", r.text());
        assert_eq!(r.header("cache-control"), "no-store");
        r.text()
    }
}

/// The hits' names, in page order.
fn hits(page: &str) -> Vec<String> {
    page.split(r#"<a class="files__link" href=""#)
        .skip(1)
        .map(|s| {
            let s = &s[s.find('>').unwrap() + 1..];
            s[..s.find("</a>").unwrap()].to_string()
        })
        .collect()
}

/// The hits' locations, in page order.
fn locations(page: &str) -> Vec<String> {
    page.split(r#"<td class="files__path"><a class="files__where" href=""#)
        .skip(1)
        .map(|s| {
            let s = &s[s.find('>').unwrap() + 1..];
            s[..s.find("</a>").unwrap()].to_string()
        })
        .collect()
}

fn summary(page: &str) -> &str {
    let at = page
        .find(r#"<p class="browser__summary results__done" role="status">"#)
        .unwrap_or_else(|| panic!("no summary in {page}"));
    let s = &page[at..];
    &s[s.find('>').unwrap() + 1..s.find("</p>").unwrap()]
}

#[tokio::test]
async fn every_word_matches_without_case_or_accents() {
    let e = env();
    e.put("medien/Halterung/Gehäuse_Deckel.stl", b"solid");
    e.put("medien/Halterung/Deckel.txt", b"x");
    e.put("medien/Rechnungen 2026/Rechnung 2026-001.pdf", b"%PDF");
    e.put("medien/Rechnungen 2025/Rechnung 2025-001.pdf", b"%PDF");
    let s = e.session("jana", "familie").await;

    let t = e.search(&s, "q=DECKEL+gehause").await;
    assert_eq!(hits(&t), ["Gehäuse_Deckel.stl"], "{t}");
    assert_eq!(locations(&t), ["Medien / Halterung"]);
    assert_eq!(summary(&t), "1 match for “DECKEL gehause” in all libraries");
    // Files open in the viewer in their folder; the location links to the folder.
    assert!(
        t.contains(
            r#"<a class="files__link" href="/Medien/Halterung/Geh%C3%A4use_Deckel.stl?view">"#
        ),
        "{t}"
    );
    assert!(
        t.contains(r#"<a class="files__where" href="/Medien/Halterung/">Medien / Halterung</a>"#),
        "{t}"
    );

    // Folders and files, all words in any order.
    let t = e.search(&s, "q=2026+rechnung").await;
    // A folder before its contents.
    assert_eq!(
        hits(&t),
        ["Rechnungen 2026", "Rechnung 2026-001.pdf"],
        "{t}"
    );
    assert!(
        t.contains(
            r#"<a class="files__link" href="/Medien/Rechnungen%202026/">Rechnungen 2026</a>"#
        ),
        "{t}"
    );
    assert!(
        t.contains(r#"<tr class="files__row" data-type="folder">"#),
        "{t}"
    );
    // German, with the accents typed.
    let t = e
        .get_h(
            "/.cc/search?q=Geh%C3%A4use",
            &[("cookie", &s.cookie), ("accept-language", "de")],
        )
        .await
        .text();
    assert_eq!(hits(&t), ["Gehäuse_Deckel.stl"], "{t}");

    // Nothing found: the empty state, no rows.
    let t = e.search(&s, "q=zzz").await;
    assert!(hits(&t).is_empty());
    assert!(t.contains("Nothing found for “zzz”."), "{t}");
}

#[tokio::test]
async fn only_readable_libraries_and_home_only_ones_only_at_home() {
    let e = env();
    e.put("medien/plan.txt", b"x");
    e.put("eingang/plan.txt", b"x");
    e.put("share/plan.txt", b"x");
    e.put("backups/plan.txt", b"x");
    // leser reads Medien; the public volume is readable for everyone.
    let leser = e.session("lena", "leser").await;
    let t = e.search(&leser, "q=plan").await;
    assert_eq!(locations(&t), ["Medien", "Share"], "{t}");

    // The admin reads everything at home; from the internet the home-only volume is out.
    let admin = e.session("Admin", "admin").await;
    let t = e.search(&admin, "q=plan").await;
    assert_eq!(
        locations(&t),
        ["Backups", "Eingang", "Medien", "Share"],
        "{t}"
    );
    let remote = e
        .send(
            "GET",
            "/.cc/search?q=plan",
            "192.0.2.10",
            &[
                ("cf-connecting-ip", "203.0.113.5"),
                ("cookie", &admin.cookie),
            ],
        )
        .await;
    assert_eq!(remote.status, StatusCode::OK, "{}", remote.text());
    assert_eq!(locations(&remote.text()), ["Eingang", "Medien", "Share"]);
    // Naming the home-only library doesn't bring it back.
    let remote = e
        .send(
            "GET",
            "/.cc/search?q=plan&vol=Backups",
            "192.0.2.10",
            &[
                ("cf-connecting-ip", "203.0.113.5"),
                ("cookie", &admin.cookie),
            ],
        )
        .await;
    assert_eq!(locations(&remote.text()), ["Eingang", "Medien", "Share"]);
}

#[tokio::test]
async fn the_current_library_comes_first() {
    let e = env();
    e.put("medien/b/plan 2.txt", b"x");
    e.put("medien/a/plan 10.txt", b"x");
    e.put("medien/a/plan 9.txt", b"x");
    e.put("eingang/plan.txt", b"x");
    let s = e.session("jana", "familie").await;
    // Sidebar order, then by path in natural order.
    let t = e.search(&s, "q=plan").await;
    assert_eq!(
        locations(&t),
        ["Eingang", "Medien / a", "Medien / a", "Medien / b"]
    );
    assert_eq!(
        hits(&t),
        ["plan.txt", "plan 9.txt", "plan 10.txt", "plan 2.txt"]
    );
    let t = e.search(&s, "q=plan&vol=Medien").await;
    assert_eq!(
        locations(&t),
        ["Medien / a", "Medien / a", "Medien / b", "Eingang"]
    );
    // The next search from this page starts there too.
    assert!(
        t.contains(r#"<input type="hidden" name="vol" value="Medien">"#),
        "{t}"
    );
    // An unknown or unreadable library changes nothing.
    let t = e.search(&s, "q=plan&vol=Backups").await;
    assert_eq!(locations(&t)[0], "Eingang");
    assert!(!t.contains(r#"name="vol""#), "{t}");
}

#[tokio::test]
async fn hidden_names_only_for_admins_who_ask_internal_names_never() {
    let e = env();
    e.put("medien/.notizen/geheim.txt", b"x");
    e.put("medien/.chestcraft-trash/geheim-alt.txt", b"x");
    e.put("medien/.chestcraft-uploads/geheim.part", b"x");
    let admin = e.session("Admin", "admin").await;
    assert!(hits(&e.search(&admin, "q=geheim").await).is_empty());
    let t = e.search(&admin, "q=geheim&hidden=1").await;
    assert_eq!(hits(&t), ["geheim.txt"], "{t}");
    assert_eq!(locations(&t), ["Medien / .notizen"]);
    // The viewer keeps the hidden files on, so it can open them.
    assert!(
        t.contains(r#"href="/Medien/.notizen/geheim.txt?view&amp;hidden=1""#),
        "{t}"
    );
    // The switch is a checkbox in the form (admins only).
    assert!(
        t.contains(r#"<input type="checkbox" name="hidden" value="1" checked>"#),
        "{t}"
    );
    let t = e.search(&admin, "q=.notizen&hidden=1").await;
    assert_eq!(hits(&t), [".notizen"]);
    assert!(hits(&e.search(&admin, "q=chestcraft&hidden=1").await).is_empty());

    let jana = e.session("jana", "familie").await;
    let t = e.search(&jana, "q=geheim&hidden=1").await;
    assert!(hits(&t).is_empty(), "{t}");
    assert!(!t.contains(r#"name="hidden""#), "{t}");
}

#[tokio::test]
async fn at_most_five_hundred_hits_then_refine() {
    let e = env();
    for i in 0..(crate::web::search::MAX_HITS + 10) {
        e.put(&format!("eingang/Messreihe/messung_{i:04}.csv"), b"1");
    }
    let s = e.session("jana", "familie").await;
    let t = e.search(&s, "q=messung").await;
    assert_eq!(hits(&t).len(), crate::web::search::MAX_HITS);
    assert_eq!(hits(&t)[0], "messung_0000.csv");
    assert_eq!(
        summary(&t),
        "More than 500 matches for “messung” in all libraries"
    );
    assert!(
        t.contains(
            r#"<p class="banner results__notice" data-state="more">Only the first 500 are shown."#
        ),
        "{t}"
    );
    // The page ends properly after the streamed rows.
    assert!(t.trim_end().ends_with("</html>"), "{t}");
}

#[tokio::test]
async fn an_empty_query_walks_nothing() {
    let e = env();
    let s = e.session("jana", "familie").await;
    for q in ["", "q=", "q=+++", "q=%09"] {
        let t = e.search(&s, q).await;
        assert!(
            t.contains(r#"<p class="browser__empty results__prompt">"#),
            "{q}: {t}"
        );
        assert!(
            !t.contains("<table") && !t.contains("results__done"),
            "{q}: {t}"
        );
    }
    // Opened from the Search tab: the empty field takes the focus; with a query it doesn't.
    let t = e.search(&s, "from=tab").await;
    assert!(t.contains(r#"data-js="find" autofocus>"#), "{t}");
    let t = e.search(&s, "from=tab&q=x").await;
    assert!(!t.contains("autofocus"), "{t}");
    let t = e.search(&s, "").await;
    assert!(!t.contains("autofocus"), "{t}");
}

#[tokio::test]
async fn visitors_have_no_search() {
    let e = env();
    // Anonymous: to the sign-in page, back to the search afterwards.
    let r = e.get("/.cc/search?q=a").await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(
        r.header("location"),
        "/.cc/login?next=%2F.cc%2Fsearch%3Fq%3Da"
    );
    // A public volume: the title row's filter, no search field, no tab bar.
    let t = e.get("/Share/").await.text();
    assert!(
        t.contains(
            r#"<input class="filter__input" type="search" placeholder="Filter" data-js="filter""#
        ),
        "{t}"
    );
    assert!(!t.contains("/.cc/search") && !t.contains("tabbar"), "{t}");
    // A share's visitors (even with an account's cookie elsewhere): no search either.
    let admin = e.session("Admin", "admin").await;
    for i in 0..20 {
        e.put(&format!("medien/Viele/x{i}.txt"), b"x");
    }
    let (id, pw) = e.create_share(&admin, "Medien", "Viele", &[]).await;
    let cookie = e.visit(&id, &pw).await;
    let t = e
        .get_h(&format!("/s/{id}/"), &[("cookie", &cookie)])
        .await
        .text();
    assert!(t.contains(r#"data-js="filter""#), "{t}");
    assert!(
        !t.contains("/.cc/search") && !t.contains(r#"class="find""#),
        "{t}"
    );
}

#[tokio::test]
async fn the_app_bar_field_filters_the_folder_and_searches_from_it() {
    let e = env();
    let s = e.session("jana", "familie").await;
    let t = e
        .get_h("/Medien/Urlaub%20Gardasee/", &[("cookie", &s.cookie)])
        .await
        .text();
    let bar = &t[t.find(r#"<header class="appbar">"#).unwrap()..t.find("</header>").unwrap()];
    assert!(
        bar.contains(r#"<form class="find" data-place="appbar" method="get" action="/.cc/search" role="search" data-js="search">"#),
        "{bar}"
    );
    assert!(
        bar.contains(r#"name="q" placeholder="Filter or search …" title="Typing filters this folder; Enter searches all libraries""#)
            && bar.contains(r#"data-js="find filter""#)
            && bar.contains(r#"<input type="hidden" name="vol" value="Medien">"#),
        "{bar}"
    );
    // Its phone copy stands in the title row; the old filter is gone there.
    let tools = &t[t.find(r#"<div class="browser__tools""#).unwrap()..];
    let tools = &tools[..tools.find("</form>").unwrap()];
    assert!(
        tools.contains(r#"<form class="find" data-place="title""#)
            && tools.contains(r#"id="find-title""#),
        "{tools}"
    );
    assert_eq!(t.matches(r#"placeholder="Filter""#).count(), 0, "{t}");
    // The shortcut overview says what Enter does.
    assert!(
        t.contains("Filter this folder (Enter searches all libraries)"),
        "{t}"
    );

    // Other pages: the field searches only, from no library.
    let t = e.get_h("/.cc/trash", &[("cookie", &s.cookie)]).await.text();
    assert!(
        t.contains(r#"placeholder="Search …""#) && t.contains(r#"data-js="find">"#),
        "{t}"
    );
    assert!(
        !t.contains(r#"name="vol""#) && !t.contains("find filter"),
        "{t}"
    );
    // The search page has its own field instead.
    let t = e.search(&s, "q=x").await;
    assert!(!t.contains(r#"class="find""#), "{t}");
    assert!(t.contains(r#"id="results-q""#), "{t}");
}

#[tokio::test]
async fn the_search_tab_starts_in_the_current_library() {
    let e = env();
    let s = e.session("jana", "familie").await;
    let tabbar = |t: &str| {
        let start = t.find(r#"<nav class="tabbar""#).unwrap();
        t[start..start + t[start..].find("</nav>").unwrap()].to_string()
    };
    let t = e.get_h("/Medien/", &[("cookie", &s.cookie)]).await.text();
    let bar = tabbar(&t);
    assert_eq!(bar.matches(r#"class="tabbar__item""#).count(), 4, "{bar}");
    let labels: Vec<&str> = bar
        .split(r#"<span class="tabbar__label">"#)
        .skip(1)
        .map(|s| &s[..s.find('<').unwrap()])
        .collect();
    assert_eq!(labels, ["Libraries", "Search", "Shares", "Account"]);
    assert!(
        bar.contains(r#"<a class="tabbar__item" href="/.cc/search?from=tab&amp;vol=Medien">"#),
        "{bar}"
    );
    // Elsewhere without a library; the search page marks its tab.
    let t = e
        .get_h("/.cc/shares", &[("cookie", &s.cookie)])
        .await
        .text();
    assert!(tabbar(&t).contains(r#"href="/.cc/search?from=tab">"#));
    let t = e.search(&s, "from=tab").await;
    assert!(
        tabbar(&t).contains(
            r#"<a class="tabbar__item" href="/.cc/search?from=tab" aria-current="page">"#
        ),
        "{t}"
    );
}
