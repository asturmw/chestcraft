//! The search page (review Q13, Q37): the field, then the hits in the browser's file table
//! with a location column. Provisional markup and classes (`results*`, `files__path`,
//! `files__where`) until the design delivers the screen; everything else is the browser's.
//!
//! The page streams (`web::search`): [`page`] renders the whole page with two markers, the
//! rows go where [`ROWS`] stands, the [`tail`] (count and notices) where [`TAIL`] stands.

use super::views::{self, Ctx, file_icon, icon};
use maud::{Markup, PreEscaped, html};

/// Where the rows go. Comments can't come from user text: maud escapes every `<`.
pub const ROWS: &str = "<!--cc:rows-->";
/// Where the count and the notices go, after the table.
pub const TAIL: &str = "<!--cc:tail-->";

pub struct SearchView {
    pub query: String,
    /// The library the search started from (searched first), kept for the next search.
    pub vol: Option<String>,
    /// Admins: the switch for hidden files, and whether it is on.
    pub admin: bool,
    pub hidden: bool,
    /// Opened from the phone's tab bar without a query: the field takes the focus.
    pub autofocus: bool,
    /// "in all libraries" or "in Fotos".
    pub scope: String,
    /// A query with words: the table follows and the rows stream in.
    pub searching: bool,
}

pub fn page(ctx: &Ctx, v: &SearchView) -> Markup {
    let title = if v.query.trim().is_empty() {
        ctx.t("search.title").to_string()
    } else {
        ctx.tf("search.page_title", &[("query", v.query.trim())])
    };
    let main = html! {
        main class="browser results" id="main" data-view="list" {
            div class="browser__pane" {
                form class="results__form" method="get" action="/.cc/search" role="search" data-js="search" {
                    label class="filter results__field" for="results-q" {
                        (icon("search"))
                        span class="vh" { (ctx.t("search.label")) }
                        input class="filter__input" id="results-q" type="search" name="q" value=(v.query)
                            placeholder=(ctx.t("search.placeholder")) autocomplete="off" spellcheck="false"
                            enterkeyhint="search" data-js="find" autofocus[v.autofocus];
                    }
                    @if let Some(vol) = &v.vol {
                        input type="hidden" name="vol" value=(vol);
                    }
                    button class="btn btn--primary results__submit" type="submit" {
                        (icon("search"))
                        span class="btn__label" { (ctx.t("search.submit")) }
                    }
                    @if v.admin {
                        label class="results__hidden" {
                            input type="checkbox" name="hidden" value="1" checked[v.hidden];
                            " " (ctx.t("search.include_hidden"))
                        }
                    }
                }
                div class="browser__head results__head" {
                    div class="browser__heading" {
                        h1 class="browser__title" { (ctx.t("search.title")) }
                        @if v.searching {
                            p class="browser__summary results__progress" {
                                (ctx.tf("search.running", &[("query", v.query.trim()), ("scope", &v.scope)]))
                            }
                        }
                    }
                }
                @if v.searching {
                    table class="files results__table" {
                        caption class="vh" { (ctx.tf("search.caption", &[("query", v.query.trim())])) }
                        thead {
                            tr class="files__head" {
                                th class="files__icon" scope="col" { span class="vh" { (ctx.t("browser.col_type")) } }
                                th class="files__name" scope="col" { (ctx.t("browser.col_name")) }
                                th class="files__path" scope="col" { (ctx.t("search.col_location")) }
                                th class="files__size" scope="col" { (ctx.t("browser.col_size")) }
                                th class="files__date" scope="col" { (ctx.t("browser.col_modified")) }
                            }
                        }
                        tbody { (PreEscaped(ROWS)) }
                    }
                    (PreEscaped(TAIL))
                } @else {
                    p class="browser__empty results__prompt" { (ctx.t("search.prompt")) }
                }
            }
        }
    };
    views::app(ctx, &title, &[], main, html! {})
}

/// One hit, ready to show.
pub struct Hit {
    pub name: String,
    pub ftype: &'static str,
    /// Folders: the folder; files: the viewer in their folder.
    pub href: String,
    pub thumb: Option<String>,
    /// "Fotos / Urlaub Gardasee" and the link to that folder.
    pub location: String,
    pub location_href: String,
    /// Files: the size; folders: empty.
    pub size: String,
    /// Phones: "2.4 MB, 14.11.2026" (folders: the date) under the location.
    pub meta: String,
    pub modified: String,
    pub modified_iso: String,
}

pub fn row(h: &Hit) -> Markup {
    html! {
        tr class="files__row" data-type=(h.ftype) {
            td class="files__icon" data-js=[h.thumb.is_some().then_some("thumb-box")] {
                @if let Some(src) = &h.thumb {
                    img class="thumb" src=(src) alt="" width="40" height="40" loading="lazy" decoding="async" data-js="thumb";
                    template data-js="thumb-fallback" { (file_icon(h.ftype)) }
                } @else {
                    (file_icon(h.ftype))
                }
            }
            td class="files__name" {
                a class="files__link" href=(h.href) { (h.name) }
                span class="files__meta" {
                    a class="files__where" href=(h.location_href) { (h.location) }
                    " " (h.meta)
                }
            }
            td class="files__path" { a class="files__where" href=(h.location_href) { (h.location) } }
            td class="files__size" { (h.size) }
            td class="files__date" { time datetime=(h.modified_iso) { (h.modified) } }
        }
    }
}

/// Why a search ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    /// Everything readable was searched.
    Done,
    /// More hits than shown.
    Full,
    /// Out of time.
    Time,
    /// Too many entries walked.
    Entries,
    /// The account started another search.
    Replaced,
    /// Too many searches running; this one never started.
    Busy,
}

pub struct Outcome {
    pub hits: usize,
    pub end: End,
    /// Libraries that could not be searched.
    pub failed: Vec<String>,
}

/// The count and the notices, after the rows. The stylesheet puts them above the table.
pub fn tail(ctx: &Ctx, v: &SearchView, o: &Outcome, max_hits: usize, max_secs: u64) -> Markup {
    let q = v.query.trim();
    let summary = if o.end == End::Full {
        ctx.tf(
            "search.found_many",
            &[
                ("count", &ctx.number(max_hits as u64)),
                ("query", q),
                ("scope", &v.scope),
            ],
        )
    } else {
        ctx.tnf(
            "search.found",
            o.hits as u64,
            &[("query", q), ("scope", &v.scope)],
        )
    };
    let notice = match o.end {
        End::Done => None,
        End::Full => Some(("more", ctx.tn("search.more", max_hits as u64))),
        End::Time => Some((
            "incomplete",
            ctx.tf("search.timeout", &[("seconds", &max_secs.to_string())]),
        )),
        End::Entries => Some(("incomplete", ctx.t("search.too_big").to_string())),
        End::Replaced => Some(("stopped", ctx.t("search.replaced").to_string())),
        End::Busy => Some(("stopped", ctx.t("search.busy").to_string())),
    };
    html! {
        p class="browser__summary results__done" role="status" { (summary) }
        @if let Some((state, text)) = notice {
            p class="banner results__notice" data-state=(state) { (text) }
        }
        @for name in &o.failed {
            p class="banner results__notice" data-state="failed" {
                (ctx.tf("search.failed", &[("volume", name)]))
            }
        }
        @if o.hits == 0 && !matches!(o.end, End::Replaced | End::Busy) {
            div class="results__empty" {
                p class="browser__empty" { (ctx.tf("search.nothing", &[("query", q)])) }
                p class="results__hint" { (ctx.t("search.nothing_hint")) }
            }
        }
    }
}
