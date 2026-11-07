//! HTML views (maud). Markup follows the UI contract (docs/ui-handoff.md, section 5). Class
//! names are provisional until the design deliverables are integrated (plan step 7).

use super::assets;
use crate::i18n::{I18n, Lang};
use maud::{DOCTYPE, Markup, html};

/// Per-request rendering context.
#[derive(Clone)]
pub struct Ctx<'a> {
    pub i18n: &'a I18n,
    pub lang: Lang,
    /// `light`, `dark`, or None to follow the system.
    pub theme: Option<&'static str>,
    pub skin: String,
    /// `frosted` or `clear`.
    pub glass: &'static str,
    /// Panel opacity step 1–5; None keeps the skin's own.
    pub opacity: Option<u8>,
    /// `vim`, `standard` or `off`.
    pub keys: &'static str,
    /// What shows behind the panels.
    pub backdrop: Backdrop,
    pub signed_in: bool,
    /// Signed in with the admin role (dashboard link).
    pub admin: bool,
    pub user_name: Option<&'a str>,
    /// CSRF token of the session, for forms.
    pub csrf: Option<&'a str>,
    pub tz: &'a jiff::tz::TimeZone,
    /// Scripts the page loads (asset paths).
    pub scripts: Vec<&'static str>,
    /// Extra stylesheets the page links (URLs).
    pub styles: Vec<String>,
    /// This page's path: pages for visitors without an account get a language switch.
    pub lang_switch: Option<String>,
}

/// The `backdrop` block: nothing, a color (from a generated stylesheet) or a picture
/// chosen by the screen's aspect ratio.
#[derive(Debug, Clone, Default)]
pub struct Backdrop {
    /// `none`, `color` or `image`.
    pub state: &'static str,
    /// Steps 0–4.
    pub dim: u8,
    pub blur: u8,
    pub pictures: Option<Pictures>,
    /// Stylesheet that sets the color.
    pub stylesheet: Option<String>,
}

/// The picture of an `image` backdrop.
#[derive(Debug, Clone)]
pub enum Pictures {
    /// URLs for wide, near-square and tall screens (a picture set).
    Shapes([String; 3]),
    /// One picture for every shape, per theme (a skin's own background). Without a light
    /// one the dark one serves both.
    Themed { dark: String, light: Option<String> },
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

    pub fn tnf(&self, key: &str, n: u64, args: &[(&str, &str)]) -> String {
        self.i18n.tn(self.lang, key, n, args)
    }

    pub fn number(&self, n: u64) -> String {
        self.i18n.number(self.lang, n)
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
        html lang=(ctx.code()) data-theme=[ctx.theme] data-skin=(ctx.skin) data-glass=(ctx.glass)
            data-opacity=[ctx.opacity] data-keys=(ctx.keys) {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                meta name="robots" content="noindex, nofollow";
                title { (title) " – chestcraft" }
                link rel="stylesheet" href=(assets::url("tokens.css"));
                link rel="stylesheet" href=(assets::url("app.css"));
                @if let Some(href) = assets::skin_css(&ctx.skin) {
                    link rel="stylesheet" href=(href);
                }
                @if let Some(href) = &ctx.backdrop.stylesheet {
                    link rel="stylesheet" href=(href);
                }
                @for href in &ctx.styles {
                    link rel="stylesheet" href=(href);
                }
                @for js in &ctx.scripts {
                    script src=(assets::url(js)) defer {}
                }
            }
            body {
                a class="skip-link" href="#main" { (ctx.t("common.skip_to_content")) }
                (backdrop(&ctx.backdrop, ctx.theme))
                header class="app-header" {
                    a class="app-header__brand" href="/" { "chestcraft" }
                    @if let (Some(name), Some(csrf)) = (ctx.user_name, ctx.csrf) {
                        @if ctx.admin {
                            a class="app-header__dashboard" href="/.cc/admin/dashboard" { (ctx.t("common.dashboard")) }
                            a class="app-header__users" href="/.cc/admin/users" { (ctx.t("common.users")) }
                        }
                        a class="app-header__trash" href="/.cc/trash" { (ctx.t("common.trash")) }
                        a class="app-header__account" href="/.cc/account" { (ctx.t("common.account")) }
                        span class="app-header__user" { (ctx.tf("common.signed_in_as", &[("name", name)])) }
                        form class="app-header__sign-out" method="post" action="/.cc/logout" {
                            input type="hidden" name="csrf" value=(csrf);
                            button type="submit" { (ctx.t("common.sign_out")) }
                        }
                    } @else if !ctx.signed_in {
                        a class="app-header__sign-in" href="/.cc/login" { (ctx.t("common.sign_in")) }
                    }
                    @if let Some(here) = &ctx.lang_switch {
                        nav class="lang-switch" aria-label=(ctx.t("common.language")) {
                            @for (lang, code, name) in ctx.i18n.languages() {
                                @if lang == ctx.lang {
                                    span class="lang-switch__item" aria-current="true" lang=(code) { (name) }
                                } @else {
                                    a class="lang-switch__item" lang=(code) hreflang=(code)
                                        href=(format!("/.cc/lang?set={code}&next={}", super::files::enc(here))) { (name) }
                                }
                            }
                        }
                    }
                }
                main id="main" class="app-main" { (main) }
            }
        }
    }
}

/// The backdrop block. `theme` (`light`, `dark` or None for the system's) picks the file
/// of a skin's own picture; with None the browser picks.
pub fn backdrop(b: &Backdrop, theme: Option<&str>) -> Markup {
    let state = if b.state.is_empty() { "none" } else { b.state };
    html! {
        div class="backdrop" data-state=(state) data-dim=(b.dim) data-blur=(b.blur) aria-hidden="true" {
            @match &b.pictures {
                Some(Pictures::Shapes([wide, square, tall])) => {
                    picture class="backdrop__picture" {
                        source media="(max-aspect-ratio: 2/3)" srcset=(tall);
                        source media="(max-aspect-ratio: 5/4)" srcset=(square);
                        img class="backdrop__img" src=(wide) alt="" decoding="async" fetchpriority="low";
                    }
                }
                Some(Pictures::Themed { dark, light }) => {
                    @let (src, source) = match (theme, light.as_deref()) {
                        (Some("light"), Some(l)) => (l, None),
                        (None, Some(l)) => (dark.as_str(), Some(l)),
                        _ => (dark.as_str(), None),
                    };
                    picture class="backdrop__picture" {
                        @if let Some(l) = source {
                            source media="(prefers-color-scheme: light)" srcset=(l);
                        }
                        img class="backdrop__img" src=(src) alt="" decoding="async" fetchpriority="low";
                    }
                }
                None => {}
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

/// The owner's health line on the landing page: verdict and number of active alerts.
pub struct HealthLine {
    pub verdict: &'static str,
    pub alerts: usize,
}

/// i18n key of a dashboard verdict.
pub fn verdict_key(verdict: &str) -> &'static str {
    match verdict {
        "ok" => "dashboard.verdict_ok",
        "warning" => "dashboard.verdict_warning",
        "critical" => "dashboard.verdict_critical",
        _ => "dashboard.verdict_stale",
    }
}

pub fn landing(ctx: &Ctx, cards: &[VolumeCard], health: Option<HealthLine>) -> Markup {
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
            @if let Some(h) = &health {
                p class="health-line" data-state=(h.verdict) {
                    a href="/.cc/admin/dashboard" {
                        (ctx.t(verdict_key(h.verdict)))
                    }
                    @if h.alerts > 0 {
                        " · " (ctx.tn("dashboard.active_count", h.alerts as u64))
                    }
                }
            }
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
    /// The item has a share that is not revoked or expired.
    pub shared: bool,
    /// Where to create a share of it.
    pub share_href: Option<String>,
    /// Icon family (`data-type`).
    pub ftype: &'static str,
    /// Grid thumbnail, for files that have one.
    pub thumb: Option<String>,
    /// Files: direct download.
    pub download_href: Option<String>,
}

/// Top of a share's pages: what is shared, by whom, until when.
pub struct ShareHead {
    pub name: String,
    pub by: String,
    pub note: Option<String>,
    pub until: Option<String>,
}

pub fn share_head(ctx: &Ctx, h: &ShareHead) -> Markup {
    html! {
        header class="share-head" {
            h1 class="share-head__name" { (h.name) }
            p class="share-head__by" { (ctx.tf("share.shared_by", &[("name", &h.by)])) }
            @if let Some(until) = &h.until {
                p class="share-head__until" { (ctx.tf("share.available_until", &[("date", until)])) }
            }
            @if let Some(note) = &h.note {
                p class="share-head__note" { (note) }
            }
        }
    }
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
    /// Where the folder's forms post to.
    pub post_href: String,
    /// CSRF token for the folder's change forms (a session's or a public uploader's).
    pub form_csrf: Option<String>,
    pub can_write: bool,
    pub can_delete: bool,
    /// Present when the visitor may upload here (needs JS).
    pub upload: Option<UploadUi>,
    /// Days until deleted items are purged (0 = kept).
    pub trash_days: u32,
    /// Set on share pages.
    pub share: Option<ShareHead>,
    /// Create a share of this folder.
    pub share_here: Option<String>,
    /// Why nothing here can be shared.
    pub share_blocked: Option<String>,
    /// Anonymous visitors: public uploads are open, enter the password here.
    pub public_upload_href: Option<String>,
    /// `list` or `grid`.
    pub layout: &'static str,
    /// Links to the list and the grid layout.
    pub layout_hrefs: [String; 2],
}

/// What the upload script needs to know about the folder.
pub struct UploadUi {
    pub volume: String,
    /// Folder path relative to the volume (or the share).
    pub dir: String,
    pub parallel: u32,
    pub csrf: String,
    /// Base of the upload API: `/.cc`, or `/s/<id>/.cc` for share visitors.
    pub api: String,
    /// Shown when uploads have a size limit.
    pub limit: Option<String>,
}

/// The upload tray and the name-clash dialog. `upload.js` fills them in; the strings it
/// shows come from `data-t-*` attributes, so all copy stays in the catalogs.
pub fn upload_ui(ctx: &Ctx, u: &UploadUi) -> Markup {
    html! {
        div class="upload" data-js="upload" hidden
            data-vol=(u.volume) data-dir=(u.dir) data-csrf=(u.csrf) data-parallel=(u.parallel)
            data-api=(u.api)
            data-t-queued=(ctx.t("upload.state_queued"))
            data-t-uploading=(ctx.t("upload.state_uploading"))
            data-t-done=(ctx.t("upload.state_done"))
            data-t-failed=(ctx.t("upload.state_failed"))
            data-t-skipped=(ctx.t("upload.state_skipped"))
            data-t-clash=(ctx.t("upload.state_clash"))
            data-t-saved-as=(ctx.t("upload.saved_as"))
            data-t-reload=(ctx.t("upload.reload")) {
            label class="upload__pick" {
                span class="upload__pick-label" { (ctx.t("browser.upload_files")) }
                input class="upload__input" type="file" multiple data-js="upload-files";
            }
            label class="upload__pick" {
                span class="upload__pick-label" { (ctx.t("browser.upload_folder")) }
                input class="upload__input" type="file" webkitdirectory multiple data-js="upload-folder";
            }
            p class="upload__hint" { (ctx.t("browser.drop_hint")) }
            @if let Some(limit) = &u.limit {
                p class="upload__limit" { (limit) }
            }
        }
        section class="upload-tray" data-js="upload-tray" data-state="idle" hidden
            aria-labelledby="upload-tray-title" {
            h2 class="upload-tray__title" id="upload-tray-title" { (ctx.t("upload.tray_title")) }
            p class="upload-tray__summary" data-js="upload-summary" role="status" {}
            ul class="upload-tray__list" data-js="upload-list" {}
        }
        dialog class="name-clash" data-js="name-clash" aria-labelledby="name-clash-title" {
            form class="name-clash__form" method="dialog" {
                h2 class="name-clash__title" id="name-clash-title" data-js="clash-title" {}
                p class="name-clash__existing" data-js="clash-existing" {}
                p class="name-clash__new" data-js="clash-new" {}
                div class="name-clash__actions" {
                    button class="name-clash__keep" type="submit" value="rename" autofocus { (ctx.t("upload.keep_both")) }
                    button class="name-clash__replace" type="submit" value="replace" data-js="clash-replace" { (ctx.t("upload.replace")) }
                    button class="name-clash__skip" type="submit" value="skip" { (ctx.t("upload.skip")) }
                }
                label class="name-clash__all" data-js="clash-all-label" {
                    input type="checkbox" data-js="clash-all";
                    " " span data-js="clash-all-text" data-t=(ctx.t("upload.apply_all")) {}
                }
            }
        }
    }
}

/// The path to a folder. `current`: the last crumb is this page (not a link).
pub fn breadcrumb(ctx: &Ctx, crumbs: &[Crumb], current: bool) -> Markup {
    html! {
        nav class="breadcrumb" aria-label=(ctx.t("browser.location")) {
            ol class="breadcrumb__list" {
                @for (i, c) in crumbs.iter().enumerate() {
                    li class="breadcrumb__item" {
                        @if current && i + 1 == crumbs.len() {
                            span aria-current="page" { (c.label) }
                        } @else {
                            a href=(c.href) { (c.label) }
                        }
                    }
                }
            }
        }
    }
}

/// File-type icon (filled in from the icon sprite once the design is integrated).
pub fn file_icon(ftype: &str) -> Markup {
    html! { span class="ficon" data-type=(ftype) aria-hidden="true" {} }
}

pub fn listing(ctx: &Ctx, v: &ListingView) -> Markup {
    layout(
        ctx,
        &v.title,
        html! {
            @if let Some(h) = &v.share {
                (share_head(ctx, h))
            }
            (breadcrumb(ctx, &v.crumbs, true))
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
                @if let (true, Some(csrf)) = (v.can_write, &v.form_csrf) {
                    form class="new-folder" method="post" action=(v.post_href) {
                        input type="hidden" name="csrf" value=(csrf);
                        input type="hidden" name="op" value="mkdir";
                        label class="field" {
                            span class="field__label" { (ctx.t("browser.folder_name")) }
                            input class="field__input" type="text" name="name" required maxlength="255"
                                autocomplete="off";
                        }
                        button class="new-folder__submit" type="submit" { (ctx.t("browser.create_folder")) }
                    }
                }
                span class="browser__count" { (ctx.tn("browser.items", v.total as u64)) }
                nav class="seg browser__layout" aria-label=(ctx.t("browser.layout")) {
                    @for (i, (name, key)) in [("list", "browser.layout_list"), ("grid", "browser.layout_grid")].iter().enumerate() {
                        @if *name == v.layout {
                            span class="seg__item" aria-current="true" { (ctx.t(key)) }
                        } @else {
                            a class="seg__item" href=(v.layout_hrefs[i]) { (ctx.t(key)) }
                        }
                    }
                }
                a class="browser__zip" href=(v.zip_href) download { (ctx.t("browser.download_folder")) }
                @if let Some((label, href)) = &v.hidden_toggle {
                    a class="browser__hidden-toggle" href=(href) { (label) }
                }
                @if let Some(href) = &v.share_here {
                    a class="browser__share" href=(href) { (ctx.t("browser.share_folder")) }
                }
                @if let Some(href) = &v.public_upload_href {
                    a class="browser__public-upload" href=(href) { (ctx.t("browser.public_upload")) }
                }
            }
            @if let Some(reason) = &v.share_blocked {
                p class="banner" data-state="share-unavailable" { (reason) }
            }
            @if let Some(u) = &v.upload {
                (upload_ui(ctx, u))
            }
            @if v.rows.is_empty() {
                p class="browser__empty" { (ctx.t("browser.empty")) }
            } @else {
                form class="browser" method="post" action=(v.post_href) data-view=(v.layout) {
                    @if let Some(csrf) = &v.form_csrf {
                        input type="hidden" name="csrf" value=(csrf);
                    }
                    @if v.showing_hidden {
                        input type="hidden" name="hidden" value="1";
                    }
                    table class="browser__table" {
                        thead {
                            tr {
                                th scope="col" class="browser__select" {
                                    span class="visually-hidden" { (ctx.t("browser.col_select")) }
                                }
                                th scope="col" class="browser__icon" {
                                    span class="visually-hidden" { (ctx.t("browser.col_type")) }
                                }
                                th scope="col" { a href=(v.sort_hrefs[0]) { (ctx.t("browser.col_name")) } }
                                th scope="col" { a href=(v.sort_hrefs[1]) { (ctx.t("browser.col_size")) } }
                                th scope="col" { a href=(v.sort_hrefs[2]) { (ctx.t("browser.col_modified")) } }
                            }
                        }
                        tbody {
                            @for r in &v.rows {
                                tr class="browser__row" data-kind=(if r.is_dir { "dir" } else { "file" }) data-type=(r.ftype) {
                                    td class="browser__select" {
                                        input type="checkbox" name="n" value=(r.name)
                                            aria-label=(ctx.tf("browser.select_item", &[("name", &r.name)]));
                                    }
                                    td class="browser__icon" {
                                        @if let Some(src) = &r.thumb {
                                            img class="thumb" src=(src) alt="" loading="lazy" decoding="async";
                                        } @else {
                                            (file_icon(r.ftype))
                                        }
                                    }
                                    th scope="row" class="browser__name" {
                                        a href=(r.href) { (r.name) }
                                        @if r.shared {
                                            " " span class="badge" data-state="shared" { (ctx.t("browser.shared")) }
                                        }
                                    }
                                    td class="browser__size" { (r.size) }
                                    td class="browser__modified" { (r.modified) }
                                    td class="browser__row-actions" {
                                        @if let Some(href) = &r.download_href {
                                            a class="browser__download" href=(href) download
                                                aria-label=(ctx.tf("browser.download_item", &[("name", &r.name)])) {
                                                (ctx.t("browser.download"))
                                            }
                                        }
                                        @if let Some(href) = &r.share_href {
                                            " "
                                            a href=(href) aria-label=(ctx.tf("browser.share_item", &[("name", &r.name)])) {
                                                (ctx.t("browser.share"))
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    div class="browser__actions" {
                        button type="submit" name="op" value="zip" class="browser__zip-selected" { (ctx.t("browser.download_selected")) }
                        @if v.can_delete {
                            button type="submit" name="op" value="delete" class="browser__delete-selected"
                                aria-describedby="trash-note" { (ctx.t("browser.delete_selected")) }
                            p class="browser__trash-note" id="trash-note" {
                                @if v.trash_days == 0 {
                                    (ctx.t("browser.trash_note_kept"))
                                } @else {
                                    (ctx.tn("browser.trash_note", u64::from(v.trash_days)))
                                }
                            }
                        }
                    }
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

/// A failed operation: what went wrong (optionally per item) and the way back.
pub fn problems(
    ctx: &Ctx,
    title: &str,
    intro: &str,
    items: &[String],
    back: Option<&str>,
) -> Markup {
    layout(
        ctx,
        title,
        html! {
            h1 class="error__title" { (title) }
            p class="error__body" role="alert" { (intro) }
            @if !items.is_empty() {
                ul class="error__items" {
                    @for i in items { li { (i) } }
                }
            }
            @if let Some(href) = back {
                p { a class="error__back" href=(href) { (ctx.t("ops.back")) } }
            }
        },
    )
}

pub struct TrashRow {
    pub id: String,
    pub name: String,
    pub is_dir: bool,
    pub location: String,
    pub location_href: String,
    pub deleted: String,
    pub by: String,
    pub size: String,
    pub until: String,
    pub note: Option<String>,
}

pub struct TrashGroup {
    pub volume: String,
    pub summary: String,
    pub rows: Vec<TrashRow>,
}

pub struct TrashView {
    pub admin: bool,
    pub csrf: String,
    pub groups: Vec<TrashGroup>,
}

pub fn trash(ctx: &Ctx, v: &TrashView) -> Markup {
    let title = ctx.t("trash.title");
    layout(
        ctx,
        title,
        html! {
            h1 class="trash__title" { (title) }
            p class="trash__intro" {
                (if v.admin { ctx.t("trash.intro_admin") } else { ctx.t("trash.intro_user") })
            }
            @if v.groups.is_empty() {
                p class="trash__empty" { (ctx.t("trash.empty")) }
            }
            @for (i, g) in v.groups.iter().enumerate() {
                section class="trash-volume" aria-labelledby=(format!("trash-volume-{i}")) {
                    h2 class="trash-volume__title" id=(format!("trash-volume-{i}")) { (g.volume) }
                    p class="trash-volume__summary" { (g.summary) }
                    @if v.admin {
                        form class="trash-volume__empty" method="post" action="/.cc/trash/empty" {
                            input type="hidden" name="csrf" value=(v.csrf);
                            input type="hidden" name="vol" value=(g.volume);
                            label class="trash-volume__confirm" {
                                input type="checkbox" name="confirm" value="1" required;
                                " " (ctx.t("trash.empty_confirm"))
                            }
                            button type="submit" { (ctx.t("trash.empty_now")) }
                        }
                    }
                    table class="trash-volume__table" {
                        thead {
                            tr {
                                th scope="col" { (ctx.t("trash.col_name")) }
                                th scope="col" { (ctx.t("trash.col_location")) }
                                th scope="col" { (ctx.t("trash.col_deleted")) }
                                @if v.admin { th scope="col" { (ctx.t("trash.col_by")) } }
                                th scope="col" { (ctx.t("trash.col_size")) }
                                th scope="col" { (ctx.t("trash.col_until")) }
                                th scope="col" { span class="visually-hidden" { (ctx.t("trash.col_actions")) } }
                            }
                        }
                        tbody {
                            @for r in &g.rows {
                                tr class="trash-item" data-kind=(if r.is_dir { "dir" } else { "file" }) {
                                    th scope="row" class="trash-item__name" {
                                        (r.name)
                                        @if let Some(n) = &r.note {
                                            " " span class="trash-item__note" { (n) }
                                        }
                                    }
                                    td class="trash-item__location" { a href=(r.location_href) { (r.location) } }
                                    td class="trash-item__deleted" { (r.deleted) }
                                    @if v.admin { td class="trash-item__by" { (r.by) } }
                                    td class="trash-item__size" { (r.size) }
                                    td class="trash-item__until" { (r.until) }
                                    td class="trash-item__actions" {
                                        form method="post" action="/.cc/trash/restore" {
                                            input type="hidden" name="csrf" value=(v.csrf);
                                            input type="hidden" name="id" value=(r.id);
                                            button type="submit"
                                                aria-label=(ctx.tf("trash.restore_item", &[("name", &r.name)])) {
                                                (ctx.t("trash.restore"))
                                            }
                                        }
                                    }
                                }
                            }
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

pub struct LoginView {
    pub next: String,
    pub username: String,
    pub error: Option<String>,
}

pub fn login(ctx: &Ctx, v: &LoginView) -> Markup {
    let title = ctx.t("login.title");
    layout(
        ctx,
        title,
        html! {
            h1 class="login__title" { (title) }
            @if let Some(e) = &v.error {
                p class="form__error" role="alert" { (e) }
            }
            form class="login" method="post" action="/.cc/login" {
                input type="hidden" name="next" value=(v.next);
                label class="field" {
                    span class="field__label" { (ctx.t("login.username")) }
                    input class="field__input" type="text" name="username" value=(v.username)
                        autocomplete="username" autocapitalize="off" spellcheck="false" required;
                }
                label class="field" {
                    span class="field__label" { (ctx.t("login.password")) }
                    input class="field__input" type="password" name="password"
                        autocomplete="current-password" required;
                }
                button class="login__submit" type="submit" { (ctx.t("login.submit")) }
            }
        },
    )
}

pub struct PasswordView {
    /// First sign-in of an account with a one-time password: no current password asked.
    pub forced: bool,
    pub csrf: String,
    pub error: Option<String>,
    pub saved: bool,
}

pub fn password(ctx: &Ctx, v: &PasswordView) -> Markup {
    let title = ctx.t("password.title");
    let min = crate::auth::password::MIN_ACCOUNT_PASSWORD.to_string();
    layout(
        ctx,
        title,
        html! {
            h1 class="password__title" { (title) }
            @if v.forced {
                p class="password__intro" { (ctx.t("password.forced_intro")) }
            }
            @if let Some(e) = &v.error {
                p class="form__error" role="alert" { (e) }
            }
            @if v.saved {
                p class="form__saved" role="status" { (ctx.t("password.saved")) }
            }
            form class="password" method="post" action="/.cc/password" {
                input type="hidden" name="csrf" value=(v.csrf);
                @if !v.forced {
                    label class="field" {
                        span class="field__label" { (ctx.t("password.current")) }
                        input class="field__input" type="password" name="current"
                            autocomplete="current-password" required;
                    }
                }
                label class="field" {
                    span class="field__label" { (ctx.t("password.new")) }
                    input class="field__input" type="password" name="new" autocomplete="new-password"
                        minlength=(min) required aria-describedby="password-hint";
                    span class="field__hint" id="password-hint" { (ctx.tf("password.hint", &[("min", &min)])) }
                }
                label class="field" {
                    span class="field__label" { (ctx.t("password.repeat")) }
                    input class="field__input" type="password" name="repeat" autocomplete="new-password"
                        minlength=(min) required;
                }
                button class="password__submit" type="submit" { (ctx.t("password.submit")) }
            }
        },
    )
}

/// What the viewer's stage shows.
pub enum ViewerBody {
    /// A picture: the large thumbnail, linking to the original.
    Image {
        src: String,
        full: String,
    },
    /// The large thumbnail of a 3D model, G-code or SVG.
    Rendered {
        src: String,
    },
    Video {
        src: String,
        poster: String,
    },
    Audio {
        src: String,
    },
    Pdf {
        src: String,
    },
    Text {
        text: String,
        truncated: bool,
    },
    /// Already sanitized HTML.
    Markdown {
        html: String,
        truncated: bool,
    },
    None,
}

impl ViewerBody {
    pub fn state(&self) -> &'static str {
        match self {
            ViewerBody::Image { .. } => "image",
            ViewerBody::Rendered { .. } => "rendered",
            ViewerBody::Video { .. } => "video",
            ViewerBody::Audio { .. } => "audio",
            ViewerBody::Pdf { .. } => "pdf",
            ViewerBody::Text { .. } => "text",
            ViewerBody::Markdown { .. } => "markdown",
            ViewerBody::None => "none",
        }
    }
}

pub struct ViewerView {
    pub name: String,
    pub ftype: &'static str,
    pub type_label: String,
    pub size: String,
    pub modified: String,
    /// "Volume / folder / folder".
    pub location: String,
    pub crumbs: Vec<Crumb>,
    pub body: ViewerBody,
    pub download_href: String,
    pub close_href: String,
    pub prev_href: Option<String>,
    pub next_href: Option<String>,
    /// (folder URL, CSRF token): the visitor may move the file to the trash.
    pub delete: Option<(String, String)>,
    pub share_href: Option<String>,
    pub share: Option<ShareHead>,
}

/// The picture, player or text of a file (viewer and single-file share).
pub fn viewer_stage(
    ctx: &Ctx,
    name: &str,
    ftype: &str,
    body: &ViewerBody,
    download: &str,
) -> Markup {
    html! {
        div class="viewer__stage" data-state=(body.state()) {
            @match body {
                ViewerBody::Image { src, full } => {
                    a class="viewer__picture" href=(full) {
                        img class="viewer__image" src=(src) alt=(name);
                    }
                    a class="viewer__full-size" href=(full) { (ctx.t("viewer.full_size")) }
                }
                ViewerBody::Rendered { src } => {
                    img class="viewer__image" src=(src) alt=(name);
                }
                ViewerBody::Video { src, poster } => {
                    video class="viewer__video" src=(src) poster=(poster) controls preload="metadata" {}
                    p class="viewer__hint" { (ctx.t("viewer.no_playback")) }
                }
                ViewerBody::Audio { src } => {
                    audio class="viewer__audio" src=(src) controls preload="metadata" {}
                    p class="viewer__hint" { (ctx.t("viewer.no_playback")) }
                }
                ViewerBody::Pdf { src } => {
                    iframe class="viewer__pdf" src=(src) title=(name) {}
                }
                ViewerBody::Text { text, truncated } => {
                    pre class="viewer__text" { (text) }
                    @if *truncated {
                        p class="viewer__hint" { (ctx.t("viewer.truncated")) }
                    }
                }
                ViewerBody::Markdown { html, truncated } => {
                    div class="viewer__markdown" { (maud::PreEscaped(html)) }
                    @if *truncated {
                        p class="viewer__hint" { (ctx.t("viewer.truncated")) }
                    }
                }
                ViewerBody::None => {
                    (file_icon(ftype))
                    p class="viewer__none" { (ctx.t("viewer.no_preview")) }
                    a class="viewer__download-big" href=(download) download { (ctx.t("viewer.download")) }
                }
            }
        }
    }
}

pub fn viewer(ctx: &Ctx, v: &ViewerView) -> Markup {
    layout(
        ctx,
        &v.name,
        html! {
            @if let Some(h) = &v.share {
                (share_head(ctx, h))
            }
            article class="viewer" data-type=(v.ftype) aria-labelledby="viewer-name" {
                header class="viewer__bar" {
                    (breadcrumb(ctx, &v.crumbs, false))
                    h1 class="viewer__name" id="viewer-name" { (v.name) }
                    nav class="viewer__nav" aria-label=(ctx.t("viewer.nav")) {
                        @if let Some(h) = &v.prev_href {
                            a class="viewer__prev" rel="prev" href=(h) { (ctx.t("viewer.prev")) }
                        }
                        @if let Some(h) = &v.next_href {
                            a class="viewer__next" rel="next" href=(h) { (ctx.t("viewer.next")) }
                        }
                    }
                    a class="viewer__close" href=(v.close_href) { (ctx.t("viewer.close")) }
                }
                (viewer_stage(ctx, &v.name, v.ftype, &v.body, &v.download_href))
                aside class="viewer__info" aria-label=(ctx.t("viewer.info")) {
                    dl {
                        dt { (ctx.t("viewer.size")) } dd { (v.size) }
                        dt { (ctx.t("viewer.modified")) } dd { (v.modified) }
                        dt { (ctx.t("viewer.location")) } dd { (v.location) }
                        dt { (ctx.t("viewer.type")) } dd { (v.type_label) }
                    }
                }
                div class="viewer__actions" {
                    a class="viewer__download" href=(v.download_href) download { (ctx.t("viewer.download")) }
                    @if let Some(href) = &v.share_href {
                        a class="viewer__share" href=(href) { (ctx.t("viewer.share")) }
                    }
                    @if let Some((folder, csrf)) = &v.delete {
                        form class="viewer__delete" method="post" action=(folder) {
                            input type="hidden" name="csrf" value=(csrf);
                            input type="hidden" name="op" value="delete";
                            input type="hidden" name="n" value=(v.name);
                            button type="submit" { (ctx.t("viewer.delete")) }
                        }
                    }
                }
            }
        },
    )
}
