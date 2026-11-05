//! HTML views (maud). Markup follows the UI contract (docs/ui-handoff.md, section 5). Class
//! names are provisional until the design deliverables are integrated (plan step 7).

use super::assets;
use crate::i18n::{I18n, Lang};
use maud::{DOCTYPE, Markup, html};

/// Per-request rendering context.
pub struct Ctx<'a> {
    pub i18n: &'a I18n,
    pub lang: Lang,
    /// `light`, `dark`, or None to follow the system.
    pub theme: Option<&'static str>,
    pub skin: &'a str,
    /// `vim`, `standard` or `off`.
    pub keys: &'static str,
    pub signed_in: bool,
    pub tz: &'a jiff::tz::TimeZone,
}

impl<'a> Ctx<'a> {
    pub fn t(&self, key: &'a str) -> &'a str {
        self.i18n.t(self.lang, key)
    }

    pub fn tf(&self, key: &str, args: &[(&str, &str)]) -> String {
        self.i18n.tf(self.lang, key, args)
    }

    pub fn tn(&self, key: &str, n: u64) -> String {
        self.i18n.tn(self.lang, key, n, &[])
    }

    pub fn size(&self, bytes: u64) -> String {
        self.i18n.size(self.lang, bytes)
    }

    pub fn datetime(&self, unix: i64) -> String {
        self.i18n.datetime(self.lang, unix, self.tz)
    }

    pub fn code(&self) -> &str {
        self.i18n.code(self.lang)
    }
}

pub fn layout(ctx: &Ctx, title: &str, main: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang=(ctx.code()) data-theme=[ctx.theme] data-skin=(ctx.skin) data-keys=(ctx.keys) {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                meta name="robots" content="noindex, nofollow";
                title { (title) " – chestcraft" }
                link rel="stylesheet" href=(assets::url("tokens.css"));
                link rel="stylesheet" href=(assets::url("app.css"));
                @if let Some(href) = assets::skin_css(ctx.skin) {
                    link rel="stylesheet" href=(href);
                }
            }
            body {
                a class="skip-link" href="#main" { (ctx.t("common.skip_to_content")) }
                header class="app-header" {
                    a class="app-header__brand" href="/" { "chestcraft" }
                    @if !ctx.signed_in {
                        a class="app-header__sign-in" href="/.cc/login" { (ctx.t("common.sign_in")) }
                    }
                }
                main id="main" class="app-main" { (main) }
            }
        }
    }
}

pub struct VolumeCard {
    pub name: String,
    pub href: String,
    pub note: Option<String>,
    pub used: Option<u64>,
    pub total: Option<u64>,
    pub free_text: Option<String>,
}

pub fn landing(ctx: &Ctx, cards: &[VolumeCard]) -> Markup {
    let title = if ctx.signed_in {
        ctx.t("landing.title")
    } else {
        ctx.t("landing.public_title")
    };
    layout(
        ctx,
        title,
        html! {
            h1 class="landing__title" { (title) }
            @if cards.is_empty() {
                p class="landing__empty" {
                    (if ctx.signed_in { ctx.t("landing.none") } else { ctx.t("landing.none_public") })
                }
            } @else {
                ul class="volume-list" {
                    @for c in cards {
                        li class="volume-list__item" {
                            a class="volume-list__link" href=(c.href) { (c.name) }
                            @if let Some(note) = &c.note {
                                p class="volume-list__note" { (note) }
                            }
                            @if let (Some(used), Some(total), Some(free)) = (c.used, c.total, &c.free_text) {
                                meter class="volume-list__meter" min="0" max=(total) value=(used) {}
                                p class="volume-list__free" { (free) }
                            }
                        }
                    }
                }
            }
        },
    )
}

pub struct Crumb {
    pub label: String,
    pub href: String,
}

pub struct Row {
    pub name: String,
    pub href: String,
    pub is_dir: bool,
    pub size: String,
    pub modified: String,
}

pub struct ListingView {
    pub title: String,
    pub crumbs: Vec<Crumb>,
    pub note: Option<String>,
    pub free: Option<String>,
    pub read_only: bool,
    pub rows: Vec<Row>,
    pub total: usize,
    pub page: usize,
    pub pages: usize,
    pub prev_href: Option<String>,
    pub next_href: Option<String>,
    /// Sort links for name, size, modified.
    pub sort_hrefs: [String; 3],
    pub zip_href: String,
    /// Label and link of the admin's show/hide hidden files switch.
    pub hidden_toggle: Option<(String, String)>,
    pub showing_hidden: bool,
}

pub fn listing(ctx: &Ctx, v: &ListingView) -> Markup {
    layout(
        ctx,
        &v.title,
        html! {
            nav class="breadcrumb" aria-label=(ctx.t("browser.location")) {
                ol class="breadcrumb__list" {
                    @for (i, c) in v.crumbs.iter().enumerate() {
                        li class="breadcrumb__item" {
                            @if i + 1 == v.crumbs.len() {
                                span aria-current="page" { (c.label) }
                            } @else {
                                a href=(c.href) { (c.label) }
                            }
                        }
                    }
                }
            }
            @if let Some(note) = &v.note {
                p class="banner" data-state="note" { (note) }
            }
            @if v.read_only {
                p class="banner" data-state="read-only" { (ctx.t("browser.read_only")) }
            }
            div class="browser__bar" {
                @if let Some(free) = &v.free {
                    span class="browser__free" { (free) }
                }
                span class="browser__count" { (ctx.tn("browser.items", v.total as u64)) }
                a class="browser__zip" href=(v.zip_href) download { (ctx.t("browser.download_folder")) }
                @if let Some((label, href)) = &v.hidden_toggle {
                    a class="browser__hidden-toggle" href=(href) { (label) }
                }
            }
            @if v.rows.is_empty() {
                p class="browser__empty" { (ctx.t("browser.empty")) }
            } @else {
                form class="browser" method="get" {
                    input type="hidden" name="zip" value="1";
                    @if v.showing_hidden {
                        input type="hidden" name="hidden" value="1";
                    }
                    table class="browser__table" {
                        thead {
                            tr {
                                th scope="col" class="browser__select" {
                                    span class="visually-hidden" { (ctx.t("browser.col_select")) }
                                }
                                th scope="col" { a href=(v.sort_hrefs[0]) { (ctx.t("browser.col_name")) } }
                                th scope="col" { a href=(v.sort_hrefs[1]) { (ctx.t("browser.col_size")) } }
                                th scope="col" { a href=(v.sort_hrefs[2]) { (ctx.t("browser.col_modified")) } }
                            }
                        }
                        tbody {
                            @for r in &v.rows {
                                tr class="browser__row" data-kind=(if r.is_dir { "dir" } else { "file" }) {
                                    td class="browser__select" {
                                        input type="checkbox" name="n" value=(r.name)
                                            aria-label=(ctx.tf("browser.select_item", &[("name", &r.name)]));
                                    }
                                    th scope="row" class="browser__name" { a href=(r.href) { (r.name) } }
                                    td class="browser__size" { (r.size) }
                                    td class="browser__modified" { (r.modified) }
                                }
                            }
                        }
                    }
                    button type="submit" class="browser__zip-selected" { (ctx.t("browser.download_selected")) }
                }
                @if v.pages > 1 {
                    nav class="pager" {
                        @if let Some(h) = &v.prev_href {
                            a class="pager__prev" rel="prev" href=(h) { (ctx.t("browser.page_prev")) }
                        }
                        span class="pager__status" {
                            (ctx.tf("browser.page_of", &[("page", &v.page.to_string()), ("pages", &v.pages.to_string())]))
                        }
                        @if let Some(h) = &v.next_href {
                            a class="pager__next" rel="next" href=(h) { (ctx.t("browser.page_next")) }
                        }
                    }
                }
            }
        },
    )
}

pub fn error_page(ctx: &Ctx, title: &str, body: &str) -> Markup {
    layout(
        ctx,
        title,
        html! {
            h1 class="error__title" { (title) }
            p class="error__body" { (body) }
            p { a class="error__home" href="/" { (ctx.t("error.back_home")) } }
        },
    )
}
