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
    pub user_name: Option<&'a str>,
    /// CSRF token of the session, for forms.
    pub csrf: Option<&'a str>,
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

    pub fn tnf(&self, key: &str, n: u64, args: &[(&str, &str)]) -> String {
        self.i18n.tn(self.lang, key, n, args)
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
                @if ctx.csrf.is_some() {
                    script src=(assets::url("upload.js")) defer {}
                }
            }
            body {
                a class="skip-link" href="#main" { (ctx.t("common.skip_to_content")) }
                header class="app-header" {
                    a class="app-header__brand" href="/" { "chestcraft" }
                    @if let (Some(name), Some(csrf)) = (ctx.user_name, ctx.csrf) {
                        a class="app-header__trash" href="/.cc/trash" { (ctx.t("common.trash")) }
                        span class="app-header__user" { (ctx.tf("common.signed_in_as", &[("name", name)])) }
                        form class="app-header__sign-out" method="post" action="/.cc/logout" {
                            input type="hidden" name="csrf" value=(csrf);
                            button type="submit" { (ctx.t("common.sign_out")) }
                        }
                    } @else if !ctx.signed_in {
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
    /// Where the folder's forms post to.
    pub post_href: String,
    pub can_write: bool,
    pub can_delete: bool,
    /// Present when the visitor may upload here (needs JS).
    pub upload: Option<UploadUi>,
    /// Days until deleted items are purged (0 = kept).
    pub trash_days: u32,
}

/// What the upload script needs to know about the folder.
pub struct UploadUi {
    pub volume: String,
    /// Folder path relative to the volume.
    pub dir: String,
    pub parallel: u32,
}

/// The upload tray and the name-clash dialog. `upload.js` fills them in; the strings it
/// shows come from `data-t-*` attributes, so all copy stays in the catalogs.
fn upload_ui(ctx: &Ctx, u: &UploadUi, csrf: &str) -> Markup {
    html! {
        div class="upload" data-js="upload" hidden
            data-vol=(u.volume) data-dir=(u.dir) data-csrf=(csrf) data-parallel=(u.parallel)
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
                @if let (true, Some(csrf)) = (v.can_write, ctx.csrf) {
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
                a class="browser__zip" href=(v.zip_href) download { (ctx.t("browser.download_folder")) }
                @if let Some((label, href)) = &v.hidden_toggle {
                    a class="browser__hidden-toggle" href=(href) { (label) }
                }
            }
            @if let (Some(u), Some(csrf)) = (&v.upload, ctx.csrf) {
                (upload_ui(ctx, u, csrf))
            }
            @if v.rows.is_empty() {
                p class="browser__empty" { (ctx.t("browser.empty")) }
            } @else {
                form class="browser" method="post" action=(v.post_href) {
                    @if let Some(csrf) = ctx.csrf {
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
