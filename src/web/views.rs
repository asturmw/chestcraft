//! HTML views (maud). Markup follows the UI contract (docs/ui-handoff.md, section 5) and the
//! design session's prototypes (`design/proto/` on the `design` branch): the app shell, the
//! folder browser and the gate pages. Screens the design hasn't delivered yet keep
//! provisional markup inside a `.panel` (styled by `assets/provisional.css`).

use super::assets;
use crate::i18n::{I18n, Lang};
use maud::{DOCTYPE, Markup, PreEscaped, html};
use std::sync::LazyLock;

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
    /// Show the app's name in the header (not on a visitor's password page).
    pub brand: bool,
    /// The sidebar (folder tree, free space, links) and the current section.
    pub nav: Option<Nav>,
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

/// Where in the app a page belongs: the tab bar marks it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Files,
    Shares,
    Trash,
    Account,
    Admin,
}

/// The sidebar: readable volumes as a tree (the current one opened along the path), the
/// current volume's free space and, for accounts, the links to shares and trash.
#[derive(Debug, Clone)]
pub struct Nav {
    pub section: Section,
    pub tree: Vec<TreeNode>,
    pub space: Option<SpaceLine>,
    /// Signed in: the number of own active shares.
    pub shares: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct TreeNode {
    pub label: String,
    pub href: String,
    /// "public", "home only".
    pub tag: Option<String>,
    /// `aria-current`: `page` for this page, `location` for the folders above it.
    pub current: Option<&'static str>,
    /// Filled for the folders on the way to the current one.
    pub children: Vec<TreeNode>,
}

#[derive(Debug, Clone)]
pub struct SpaceLine {
    pub volume: String,
    /// "1.2 TB free of 3.6 TB".
    pub free: String,
    /// 0–100.
    pub used_percent: u8,
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

    pub fn date(&self, unix: i64) -> String {
        self.i18n.date(self.lang, unix, self.tz)
    }

    /// Local time for a `<time datetime>` attribute: `2026-11-04T21:14`.
    pub fn iso_local(&self, unix: i64) -> String {
        jiff::Timestamp::from_second(unix)
            .map(|ts| {
                ts.to_zoned(self.tz.clone())
                    .strftime("%Y-%m-%dT%H:%M")
                    .to_string()
            })
            .unwrap_or_default()
    }

    pub fn code(&self) -> &str {
        self.i18n.code(self.lang)
    }

    /// A visitor's password page (share password, public-upload unlock): no brand and no
    /// language switch; the language comes from the cookie or the browser.
    pub fn gate(&self) -> Ctx<'a> {
        Ctx {
            brand: false,
            lang_switch: None,
            ..self.clone()
        }
    }

    pub fn with_nav(self, nav: Nav) -> Ctx<'a> {
        Ctx {
            nav: Some(nav),
            ..self
        }
    }
}

static ICONS: LazyLock<String> = LazyLock::new(|| assets::url("icons.svg"));

/// An icon from the sprite (`i-<id>`), decorative.
pub fn icon(id: &str) -> Markup {
    icon_c(id, "")
}

/// An icon with extra classes.
pub fn icon_c(id: &str, extra: &str) -> Markup {
    let class = if extra.is_empty() {
        "icon".to_string()
    } else {
        format!("icon {extra}")
    };
    // Ids are names from the sprite, never user input; anything else would need escaping.
    let id = if id.bytes().all(|b| b.is_ascii_lowercase() || b == b'-') {
        id
    } else {
        "file"
    };
    html! {
        svg class=(class) aria-hidden="true" {
            (PreEscaped(format!(r#"<use href="{}#i-{id}"/>"#, *ICONS)))
        }
    }
}

/// The skeleton every page shares: head, skip link, backdrop.
fn document(ctx: &Ctx, title: &str, body_class: &str, body: Markup) -> Markup {
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
                link rel="stylesheet" href=(assets::url("provisional.css"));
                @if let Some(href) = &ctx.backdrop.stylesheet {
                    link rel="stylesheet" href=(href);
                }
                @for href in &ctx.styles {
                    link rel="stylesheet" href=(href);
                }
                script src=(assets::url("app.js")) defer {}
                @for js in &ctx.scripts {
                    script src=(assets::url(js)) defer {}
                }
            }
            body class=(body_class) {
                a class="skip" href="#main" { (ctx.t("common.skip_to_content")) }
                (backdrop(&ctx.backdrop, ctx.theme))
                (body)
            }
        }
    }
}

/// A page of the app: app bar, sidebar (when the page has one), the `main` element the
/// caller renders (with `id="main"`), anything that floats outside it, and the phone tab
/// bar for accounts.
pub fn app(ctx: &Ctx, title: &str, crumbs: &[Crumb], main: Markup, floating: Markup) -> Markup {
    document(
        ctx,
        title,
        "page page--app",
        html! {
            div class="shell" {
                (appbar(ctx, crumbs))
                div class="layout" {
                    @if let Some(nav) = &ctx.nav {
                        (sidenav(ctx, nav))
                    }
                    (main)
                }
                (floating)
                @if ctx.csrf.is_some() {
                    (tabbar(ctx))
                }
            }
        },
    )
}

/// A page whose screen isn't designed yet: its markup inside a panel.
pub fn layout(ctx: &Ctx, title: &str, main: Markup) -> Markup {
    app(
        ctx,
        title,
        &[],
        html! { main id="main" class="panel" { (main) } },
        html! {},
    )
}

/// A gate page (sign-in, password pages): one card in the middle. The sign-in page keeps
/// the brand and the language switch; visitors' password pages have neither.
pub fn gate(ctx: &Ctx, title: &str, emblem: &str, card: Markup) -> Markup {
    document(
        ctx,
        title,
        "page page--gate",
        html! {
            main class="gate" id="main" {
                @if ctx.brand {
                    a class="brand gate__brand" href="/" {
                        (icon_c("chest", "brand__mark"))
                        span class="brand__name" { "chestcraft" }
                    }
                }
                @if let Some(here) = &ctx.lang_switch {
                    (langswitch(ctx, here))
                }
                section class="gate__card" aria-labelledby="gate-title" {
                    span class="gate__emblem" aria-hidden="true" { (icon(emblem)) }
                    h1 class="gate__title" id="gate-title" { (title) }
                    (card)
                }
            }
        },
    )
}

pub fn langswitch(ctx: &Ctx, here: &str) -> Markup {
    html! {
        nav class="langswitch" aria-label=(ctx.t("common.language")) {
            @for (lang, code, name) in ctx.i18n.languages() {
                @if lang == ctx.lang {
                    span class="langswitch__item" aria-current="true" lang=(code) { (name) }
                } @else {
                    a class="langswitch__item" lang=(code) hreflang=(code)
                        href=(format!("/.cc/lang?set={code}&next={}", super::files::enc(here))) { (name) }
                }
            }
        }
    }
}

fn appbar(ctx: &Ctx, crumbs: &[Crumb]) -> Markup {
    html! {
        header class="appbar" {
            @if ctx.brand {
                a class="brand" href="/" {
                    (icon_c("chest", "brand__mark"))
                    span class="brand__name" { "chestcraft" }
                }
            }
            @if !crumbs.is_empty() {
                nav class="crumbs" aria-label=(ctx.t("browser.location")) {
                    ol class="crumbs__list" {
                        @for (i, c) in crumbs.iter().enumerate() {
                            li class="crumbs__item" {
                                a class="crumbs__link" href=(c.href)
                                    aria-current=[(i + 1 == crumbs.len()).then_some("page")] { (c.label) }
                            }
                        }
                    }
                }
            }
            div class="appbar__tools" {
                @if ctx.admin {
                    a class="appbar__link appbar__link--admin" href="/.cc/admin/dashboard" {
                        (icon("shield"))
                        span class="appbar__label" { (ctx.t("nav.admin")) }
                    }
                }
                @if let (Some(name), Some(csrf)) = (ctx.user_name, ctx.csrf) {
                    (account_menu(ctx, name, csrf))
                } @else if !ctx.signed_in && ctx.brand {
                    a class="appbar__link" href="/.cc/login" {
                        (icon("user"))
                        span class="appbar__label" { (ctx.t("common.sign_in")) }
                    }
                }
                @if let Some(here) = &ctx.lang_switch {
                    (langswitch(ctx, here))
                }
            }
        }
    }
}

/// The account button and its menu (provisional items until the design delivers the
/// account menu).
fn account_menu(ctx: &Ctx, name: &str, csrf: &str) -> Markup {
    let initial: String = name
        .chars()
        .next()
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default();
    let item = |href: &str, icon_id: &str, label: &str| {
        html! {
            a class="menu__item" role="menuitem" href=(href) {
                (icon(icon_id))
                span class="menu__text" { span class="menu__label" { (label) } }
            }
        }
    };
    html! {
        button class="account" type="button" aria-haspopup="menu" popovertarget="account-menu"
            aria-label=(ctx.tf("common.signed_in_as", &[("name", name)])) {
            span class="account__avatar" aria-hidden="true" { (initial) }
            span class="account__name" aria-hidden="true" { (name) }
        }
        div class="menu account__menu" id="account-menu" role="menu" aria-label=(ctx.t("nav.account_menu")) popover {
            (item("/.cc/account", "user", ctx.t("nav.account")))
            (item("/.cc/shares", "share", ctx.t("nav.shares")))
            (item("/.cc/trash", "trash", ctx.t("nav.trash")))
            @if ctx.admin {
                (item("/.cc/admin/dashboard", "shield", ctx.t("dashboard.title")))
                (item("/.cc/admin/alerts", "warning", ctx.t("alerts.title")))
                (item("/.cc/admin/users", "user", ctx.t("users.title")))
                (item("/.cc/admin/volumes", "drive", ctx.t("volumes.title")))
                (item("/.cc/admin/appearance", "image", ctx.t("appearance.title")))
                (item("/.cc/admin/audit", "list", ctx.t("audit.title")))
            }
            form class="account__sign-out" method="post" action="/.cc/logout" {
                input type="hidden" name="csrf" value=(csrf);
                button class="menu__item" type="submit" role="menuitem" {
                    (icon("x"))
                    span class="menu__text" { span class="menu__label" { (ctx.t("common.sign_out")) } }
                }
            }
        }
    }
}

fn tree(nodes: &[TreeNode]) -> Markup {
    html! {
        @for n in nodes {
            li class="tree__item" data-state=[(!n.children.is_empty()).then_some("open")] {
                a class="tree__link" href=(n.href) aria-current=[n.current] {
                    (icon("folder"))
                    (n.label)
                    @if let Some(tag) = &n.tag {
                        " " span class="tree__tag" { (tag) }
                    }
                }
                @if !n.children.is_empty() {
                    ul class="tree__children" { (tree(&n.children)) }
                }
            }
        }
    }
}

fn sidenav(ctx: &Ctx, nav: &Nav) -> Markup {
    html! {
        nav class="sidenav" aria-label=(ctx.t("nav.volumes")) {
            h2 class="sidenav__title" { (ctx.t("nav.volumes")) }
            ul class="tree" { (tree(&nav.tree)) }
            @if let Some(sp) = &nav.space {
                div class="space" {
                    p class="space__label" {
                        span class="space__vol" { (sp.volume) }
                        " "
                        span class="space__free" { (sp.free) }
                    }
                    svg class="bar" viewBox="0 0 100 6" preserveAspectRatio="none" role="img"
                        aria-label=(ctx.tf("nav.space_used", &[("percent", &sp.used_percent.to_string())])) {
                        rect class="bar__track" width="100" height="6" {}
                        rect class="bar__fill" width=(sp.used_percent) height="6" {}
                    }
                }
            }
            @if let Some(count) = nav.shares {
                ul class="sidenav__links" {
                    li {
                        a class="sidenav__link" href="/.cc/shares"
                            aria-current=[(nav.section == Section::Shares).then_some("page")] {
                            (icon("share"))
                            (ctx.t("nav.shares"))
                            @if count > 0 {
                                " " span class="sidenav__count" { (ctx.number(count as u64)) }
                            }
                        }
                    }
                    li {
                        a class="sidenav__link" href="/.cc/trash"
                            aria-current=[(nav.section == Section::Trash).then_some("page")] {
                            (icon("trash"))
                            (ctx.t("nav.trash"))
                        }
                    }
                }
            }
        }
    }
}

fn tabbar(ctx: &Ctx) -> Markup {
    let section = ctx.nav.as_ref().map(|n| n.section);
    let tabs = [
        ("/", "folder", "nav.volumes", Section::Files),
        ("/.cc/shares", "share", "nav.shares_tab", Section::Shares),
        ("/.cc/trash", "trash", "nav.trash", Section::Trash),
        ("/.cc/account", "user", "nav.account", Section::Account),
    ];
    html! {
        nav class="tabbar" aria-label=(ctx.t("nav.sections")) {
            @for (href, icon_id, key, s) in tabs {
                a class="tabbar__item" href=(href) aria-current=[(section == Some(s)).then_some("page")] {
                    (icon(icon_id))
                    span class="tabbar__label" { (ctx.t(key)) }
                }
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
    /// Bytes (files); folders: 0.
    pub bytes: u64,
    /// Size column: the file size, or a folder's item count.
    pub size: String,
    /// Phone line under the name: size or item count, and the date.
    pub meta: String,
    pub modified: String,
    /// For `<time datetime>`.
    pub modified_iso: String,
    /// The item has a share that is not revoked or expired: the badge's tooltip.
    pub shared: Option<String>,
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
    /// "14 items, 296.7 MB".
    pub summary: String,
    /// Screen readers: what the table shows and how it is sorted.
    pub caption: String,
    pub read_only: bool,
    pub rows: Vec<Row>,
    pub page: usize,
    pub pages: usize,
    pub prev_href: Option<String>,
    pub next_href: Option<String>,
    /// Sort links for name, size, modified.
    pub sort_hrefs: [String; 3],
    /// The sorted column (0 name, 1 size, 2 modified) and whether it is descending.
    pub sorted: (usize, bool),
    /// This folder's address (zip and tree go here).
    pub folder_href: String,
    /// Free download slots of this visitor: `/.cc/dl-slots`, or the share's.
    pub slots_href: String,
    /// Label and link of the admin's show/hide hidden files switch.
    pub hidden_toggle: Option<(String, String)>,
    pub showing_hidden: bool,
    /// Where the folder's forms post to.
    pub post_href: String,
    /// CSRF token for the folder's change forms (a session's or a public uploader's).
    pub form_csrf: Option<String>,
    pub can_write: bool,
    pub can_delete: bool,
    /// Rename and move items of this folder (`m`).
    pub can_move: bool,
    /// Signed in: the volume for rename, move, copy and the clipboard (`web/ops.rs`).
    pub ops_vol: Option<String>,
    /// Present when the visitor may upload here (needs JS).
    pub upload: Option<UploadUi>,
    /// Days until deleted items are purged (0 = kept).
    pub trash_days: u32,
    /// Set on share pages.
    pub share: Option<ShareHead>,
    /// Signed in and allowed to share here: `/.cc/shares/new?vol=…&path=` (the item's path
    /// relative to the volume follows).
    pub share_base: Option<String>,
    /// This folder relative to its volume (for share links of its items).
    pub dir: String,
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
/// shows come from `data-t-*` attributes, so all copy stays in the catalogs. `bar`: the
/// action bar's Upload button picks the files, so the pickers stay hidden.
pub fn upload_ui(ctx: &Ctx, u: &UploadUi, bar: bool) -> Markup {
    html! {
        div class="upload" data-js="upload" hidden
            data-vol=(u.volume) data-dir=(u.dir) data-csrf=(u.csrf) data-parallel=(u.parallel)
            data-api=(u.api) data-pick=[bar.then_some("bar")]
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

/// The path to a folder (viewer, provisional pages). `current`: the last crumb is this
/// page (not a link).
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

/// Sprite id of a file-type icon (`data-type` values from `fs::filetype`).
pub fn type_icon(ftype: &str) -> &'static str {
    match ftype {
        "folder" => "folder",
        "image" => "image",
        "video" => "video",
        "audio" => "audio",
        "pdf" => "pdf",
        "text" => "text",
        "doc" => "doc",
        "sheet" => "sheet",
        "archive" => "archive",
        "code" => "code",
        "model" => "model",
        "gcode" => "gcode",
        "vector" => "vector",
        "disk" => "disk",
        _ => "file",
    }
}

/// File-type icon tile.
pub fn file_icon(ftype: &str) -> Markup {
    html! { span class="ficon" data-type=(ftype) { (icon(type_icon(ftype))) } }
}

/// The download menu: save to a folder, separately, or as one zip (`dl-save` and `dl-each`
/// need the page script, which unhides them). `zip` is a plain GET form, so the zip works
/// without JS. `phone`: the icon-only copy in the title row.
fn download_menu(ctx: &Ctx, id: &str, v: &ListingView, phone: bool) -> Markup {
    let label = ctx.t("browser.dl_folder");
    let zip_name = format!("{}.zip", v.title);
    html! {
        @if phone {
            button class="btn btn--icon dl__toggle" type="button" aria-haspopup="menu" popovertarget=(id)
                aria-label=(label) data-js="dl-toggle" {
                (icon("download"))
            }
        } @else {
            button class="btn btn--quiet dl__toggle" type="button" aria-haspopup="menu" popovertarget=(id)
                aria-label=(label) data-js="dl-toggle" {
                (icon("download"))
                span class="btn__label" aria-hidden="true" { (label) }
                (icon_c("chevron-down", "dl__caret"))
            }
        }
        div class="menu dl__menu" id=(id) role="menu" aria-label=(label) popover data-scope="folder" {
            (menu_button("dl-save", "folder", ctx.t("browser.dl_save"), ctx.t("browser.dl_save_hint"), true))
            (menu_button("dl-each", "download", ctx.t("browser.dl_each"), ctx.t("browser.dl_each_hint_folder"), true))
            form class="dl__form" method="get" action=(v.folder_href) {
                input type="hidden" name="zip" value="";
                @if v.showing_hidden {
                    input type="hidden" name="hidden" value="1";
                }
                button class="menu__item" type="submit" role="menuitem" data-js="dl-zip" {
                    (icon("zip"))
                    span class="menu__text" {
                        span class="menu__label" { (ctx.t("browser.dl_zip")) }
                        span class="menu__hint" { (zip_name) }
                    }
                }
            }
        }
    }
}

fn menu_button(js: &str, icon_id: &str, label: &str, hint: &str, hidden: bool) -> Markup {
    html! {
        button class="menu__item" type="button" role="menuitem" data-js=(js) hidden[hidden] {
            (icon(icon_id))
            span class="menu__text" {
                span class="menu__label" { (label) }
                span class="menu__hint" data-js="hint" { (hint) }
            }
        }
    }
}

/// The folder browser (`proto/browser.html`): title row with filter, download and view
/// toggle; the action bar; the file table; the status bar with the selection's actions.
/// The selection posts through the `sel` form (zip, delete); the page script runs it.
pub fn listing(ctx: &Ctx, v: &ListingView) -> Markup {
    let ctx = &Ctx {
        scripts: {
            let mut s = ctx.scripts.clone();
            s.push("browser.js");
            s.push("download.js");
            if v.ops_vol.is_some() {
                s.push("ops.js");
            }
            s.push("keys.js");
            s
        },
        ..ctx.clone()
    };
    let keys = ctx.keys != "off";
    let cols = [
        ("browser.col_name", "files__name"),
        ("browser.col_size", "files__size"),
        ("browser.col_modified", "files__date"),
    ];
    let trash_note = if v.trash_days == 0 {
        ctx.t("browser.trash_note_kept").to_string()
    } else {
        ctx.tn("browser.trash_note", u64::from(v.trash_days))
    };
    let main = html! {
        main class="browser" id="main" data-view=(v.layout) data-js="browser"
            data-share=[v.share_base.as_deref()] data-dir=(v.dir)
            data-t-selected=(ctx.t("browser.selected"))
            data-t-items-one=(ctx.t("browser.items_one")) data-t-items-other=(ctx.t("browser.items_other"))
            data-t-delete-one=(ctx.t("browser.delete_title_one"))
            data-t-delete-other=(ctx.t("browser.delete_title_other"))
            data-t-delete-name=(ctx.t("browser.delete_title_name"))
            data-t-dl-file=(ctx.t("browser.download")) data-t-dl-folder=(ctx.t("browser.dl_zip"))
            data-t-share=(ctx.t("browser.share")) data-t-delete=(ctx.t("browser.delete"))
            data-decimal=(ctx.t("format.decimal"))
            data-slots=(v.slots_href) data-folder=(v.title) data-hidden=[v.showing_hidden.then_some("1")]
            data-t-each-one=(ctx.t("browser.dl_each_hint_one")) data-t-each-other=(ctx.t("browser.dl_each_hint_other"))
            data-t-many=(ctx.t("browser.dl_many_title"))
            data-t-queue-count=(ctx.t("browser.queue_count")) data-t-queue-started=(ctx.t("browser.queue_started"))
            data-t-queue-saved=(ctx.t("browser.queue_saved")) data-t-queue-stop=(ctx.t("browser.queue_stop"))
            data-t-queue-close=(ctx.t("browser.queue_close")) data-t-save-done=(ctx.t("browser.save_done"))
            data-t-save-failed=(ctx.t("browser.save_failed")) data-t-save-denied=(ctx.t("browser.save_denied"))
            data-t-list-failed=(ctx.t("browser.list_failed"))
            data-vol=[v.ops_vol.as_deref()] data-csrf=[v.ops_vol.as_ref().and(v.form_csrf.as_deref())]
            data-can-write=[v.can_write.then_some("1")] data-can-move=[v.can_move.then_some("1")]
            data-t-paste-one=(ctx.t("browser.paste_label_one")) data-t-paste-other=(ctx.t("browser.paste_label_other"))
            data-t-clip-copy=(ctx.t("browser.clip_clear_copy")) data-t-clip-cut=(ctx.t("browser.clip_clear_cut"))
            data-t-move-one=(ctx.t("browser.move_title_one")) data-t-move-other=(ctx.t("browser.move_title_other"))
            data-t-copy-one=(ctx.t("browser.copy_title_one")) data-t-copy-other=(ctx.t("browser.copy_title_other"))
            data-t-move-here=(ctx.t("browser.move_here")) data-t-copy-here=(ctx.t("browser.copy_here"))
            data-t-volumes=(ctx.t("browser.picker_volumes")) data-t-picker-empty=(ctx.t("browser.picker_empty"))
            data-t-moved=(ctx.t("browser.queue_moved")) data-t-copied=(ctx.t("browser.queue_copied"))
            data-t-rename=(ctx.t("browser.rename")) data-t-move=(ctx.t("browser.move")) data-t-copy=(ctx.t("browser.copy")) {
            div class="browser__pane" {
                @if let Some(h) = &v.share {
                    (share_head(ctx, h))
                }
                @if v.read_only {
                    p class="banner" data-state="read-only" { (ctx.t("browser.read_only")) }
                }
                div class="browser__head" {
                    div class="browser__heading" {
                        h1 class="browser__title" { (v.title) }
                        p class="browser__summary" { (v.summary) }
                    }
                    div class="browser__tools" {
                        label class="filter" {
                            (icon("filter"))
                            span class="vh" { (ctx.t("browser.filter")) }
                            input class="filter__input" type="search" placeholder=(ctx.t("browser.filter_placeholder"))
                                data-js="filter" autocomplete="off";
                            @if keys {
                                kbd class="filter__key" { "/" }
                            }
                        }
                        div class="dl browser__download" { (download_menu(ctx, "dl-folder-m", v, true)) }
                        div class="seg" role="group" aria-label=(ctx.t("browser.layout")) {
                            @for (i, (name, key, icon_id)) in [("list", "browser.layout_list", "list"), ("grid", "browser.layout_grid", "grid")].iter().enumerate() {
                                a class="seg__item btn btn--icon" href=(v.layout_hrefs[i]) aria-label=(ctx.t(key))
                                    aria-current=[(*name == v.layout).then_some("true")] { (icon(icon_id)) }
                            }
                        }
                    }
                }
                div class="actionbar" role="toolbar" aria-label=(ctx.t("browser.folder_actions")) {
                    @if v.upload.is_some() {
                        button class="btn btn--primary actionbar__upload" type="button" data-js="upload-pick" {
                            (icon("upload"))
                            span class="btn__label" { (ctx.t("browser.upload")) }
                        }
                    } @else if let Some(href) = &v.public_upload_href {
                        a class="btn btn--primary actionbar__upload" href=(href) {
                            (icon("upload"))
                            span class="btn__label" { (ctx.t("browser.public_upload")) }
                        }
                    }
                    @if v.can_write && v.form_csrf.is_some() {
                        button class="btn actionbar__newfolder" type="button" data-js="new-folder"
                            aria-label=(ctx.t("browser.folder_name")) {
                            (icon("folder-plus"))
                            span class="btn__label" aria-hidden="true" { (ctx.t("browser.folder_name")) }
                        }
                    }
                    @if v.ops_vol.is_some() && v.can_write {
                        span class="actionbar__paste" data-js="paste" hidden {
                            button class="btn" type="button" data-js="paste-go" {
                                (icon("clipboard"))
                                span class="btn__label" aria-hidden="true" {
                                    strong data-js="paste-count" {}
                                    span class="actionbar__verb" { " " (ctx.t("browser.paste_verb")) }
                                }
                            }
                            button class="btn btn--icon" type="button" data-js="clip-clear" { (icon("x")) }
                        }
                    }
                    div class="dl actionbar__download" { (download_menu(ctx, "dl-folder", v, false)) }
                }
                @if let Some(reason) = &v.share_blocked {
                    p class="banner" data-state="share-unavailable" { (reason) }
                }
                @if let Some(u) = &v.upload {
                    (upload_ui(ctx, u, true))
                }
                @if v.rows.is_empty() {
                    p class="browser__empty" { (ctx.t("browser.empty")) }
                } @else {
                    table class="files" data-js="files" {
                        caption class="vh" { (v.caption) }
                        thead {
                            tr class="files__head" {
                                th class="files__check" scope="col" {
                                    input type="checkbox" aria-label=(ctx.t("browser.select_all")) data-js="select-all";
                                }
                                th class="files__icon" scope="col" { span class="vh" { (ctx.t("browser.col_type")) } }
                                @for (i, (key, class)) in cols.iter().enumerate() {
                                    @let sorted = v.sorted.0 == i;
                                    th class=(class) scope="col"
                                        aria-sort=[sorted.then_some(if v.sorted.1 { "descending" } else { "ascending" })] {
                                        a class="files__sort" href=(v.sort_hrefs[i]) {
                                            (ctx.t(key))
                                            @if sorted {
                                                " " (icon(if v.sorted.1 { "chevron-down" } else { "arrow-up" }))
                                            }
                                        }
                                    }
                                }
                                th class="files__menu" scope="col" { span class="vh" { (ctx.t("browser.col_actions")) } }
                            }
                        }
                        tbody {
                            @for r in &v.rows {
                                (file_row(ctx, r))
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
            footer class="status" data-state="idle" data-js="status" {
                div class="status__group status__group--idle" {
                    span class="status__item" { (v.summary) }
                    @if let Some((label, href)) = &v.hidden_toggle {
                        span class="status__item" { a href=(href) { (label) } }
                    }
                    @if keys {
                        span class="status__item status__item--keys" { kbd { "?" } " " (ctx.t("browser.shortcuts")) }
                    }
                }
                div class="status__group status__group--select" role="toolbar" aria-label=(ctx.t("browser.selection")) {
                    p class="status__count" {
                        strong data-js="sel-count" { "0" }
                        " "
                        span { (ctx.t("browser.selected")) span class="status__size" data-js="sel-size" {} }
                    }
                    div class="status__actions" {
                        div class="dl" {
                            button class="btn dl__toggle" type="button" aria-haspopup="menu" popovertarget="dl-sel" data-js="dl-toggle" {
                                (icon("download"))
                                span class="btn__label" {
                                    span class="lbl-long" { (ctx.t("browser.dl_toggle")) }
                                    span class="lbl-short" { (ctx.t("browser.dl_toggle_short")) }
                                }
                                (icon_c("chevron-down", "dl__caret"))
                            }
                            div class="menu dl__menu" id="dl-sel" role="menu" aria-label=(ctx.t("browser.dl_selection")) popover data-scope="selection" {
                                (menu_button("dl-save", "folder", ctx.t("browser.dl_save"), ctx.t("browser.dl_save_hint"), true))
                                (menu_button("dl-each", "download", ctx.t("browser.dl_each"), "", true))
                                (menu_button("dl-zip", "zip", ctx.t("browser.dl_zip"), "", false))
                            }
                        }
                        @if v.ops_vol.is_some() && v.can_move {
                            button class="btn" type="button" data-js="move" {
                                (icon("move"))
                                span class="btn__label" { (ctx.t("browser.move")) }
                            }
                        }
                        @if v.ops_vol.is_some() {
                            button class="btn status__overflow" type="button" data-js="copy" {
                                (icon("copy"))
                                span class="btn__label" { (ctx.t("browser.copy")) }
                            }
                        }
                        @if v.ops_vol.is_some() && v.can_move {
                            button class="btn status__overflow" type="button" data-js="rename" hidden {
                                (icon("rename"))
                                span class="btn__label" { (ctx.t("browser.rename")) }
                            }
                        }
                        @if v.share_base.is_some() {
                            button class="btn" type="button" data-js="share" {
                                (icon("share"))
                                span class="btn__label" { (ctx.t("browser.share")) }
                            }
                        }
                        @if v.can_delete {
                            button class="btn btn--danger" type="button" data-js="delete" {
                                (icon("trash"))
                                span class="btn__label" { (ctx.t("browser.delete")) }
                            }
                        }
                        @if v.ops_vol.is_some() {
                            button class="btn status__more" type="button" aria-haspopup="menu" popovertarget="sel-more" {
                                (icon("more"))
                                span class="btn__label" { (ctx.t("browser.more")) }
                            }
                            div class="menu" id="sel-more" role="menu" aria-label=(ctx.t("browser.more")) popover {
                                button class="menu__item" type="button" role="menuitem" data-js="copy" {
                                    (icon("copy"))
                                    span class="menu__text" { span class="menu__label" { (ctx.t("browser.copy")) } }
                                }
                                @if v.can_move {
                                    button class="menu__item" type="button" role="menuitem" data-js="rename" {
                                        (icon("rename"))
                                        span class="menu__text" { span class="menu__label" { (ctx.t("browser.rename")) } }
                                    }
                                }
                            }
                        }
                    }
                    button class="btn btn--icon status__close" type="button" data-js="sel-clear"
                        aria-label=(ctx.t("browser.clear_selection")) { (icon("x")) }
                }
            }
            form id="sel" method="post" action=(v.post_href) hidden data-js="sel-form" {
                @if let Some(csrf) = &v.form_csrf {
                    input type="hidden" name="csrf" value=(csrf);
                }
                @if v.showing_hidden {
                    input type="hidden" name="hidden" value="1";
                }
                input type="hidden" name="op" value="zip" data-js="sel-op";
            }
            div class="menu" id="row-menu" role="menu" popover data-js="row-menu" {}
            @if v.can_write && v.form_csrf.is_some() {
                dialog class="dialog" data-js="new-folder-dialog" aria-labelledby="new-folder-title" {
                    form class="dialog__form" method="post" action=(v.post_href) {
                        h2 class="dialog__title" id="new-folder-title" { (ctx.t("browser.folder_name")) }
                        @if let Some(csrf) = &v.form_csrf {
                            input type="hidden" name="csrf" value=(csrf);
                        }
                        input type="hidden" name="op" value="mkdir";
                        div class="field" {
                            label class="field__label" for="new-folder-name" { (ctx.t("browser.folder_name_label")) }
                            div class="field__control" {
                                input class="field__input" id="new-folder-name" type="text" name="name" required
                                    maxlength="255" autocomplete="off";
                            }
                        }
                        div class="dialog__actions" {
                            button class="btn" type="button" data-js="dialog-cancel" { (ctx.t("common.cancel")) }
                            button class="btn btn--primary" type="submit" { (ctx.t("browser.create_folder")) }
                        }
                    }
                }
            }
            dialog class="dialog" data-js="dl-many-dialog" aria-labelledby="dl-many-title" {
                form class="dialog__form" method="dialog" {
                    h2 class="dialog__title" id="dl-many-title" data-js="dl-many-title" {}
                    p class="dialog__text" { (ctx.t("browser.dl_many_text")) }
                    div class="dialog__actions" {
                        button class="btn" type="submit" value="cancel" { (ctx.t("common.cancel")) }
                        button class="btn btn--primary" type="submit" value="go" autofocus { (ctx.t("browser.dl_many_go")) }
                    }
                }
            }
            @if v.ops_vol.is_some() {
                (ops_dialogs(ctx))
            }
            @if ctx.keys != "off" {
                (keys_help(ctx))
            }
            @if v.can_delete {
                dialog class="dialog" data-js="delete-dialog" aria-labelledby="delete-title" {
                    form class="dialog__form" method="dialog" {
                        h2 class="dialog__title" id="delete-title" data-js="delete-title" {}
                        p class="dialog__text" { (trash_note) }
                        div class="dialog__actions" {
                            button class="btn" type="submit" value="cancel" { (ctx.t("common.cancel")) }
                            button class="btn btn--danger" type="submit" value="delete" autofocus { (ctx.t("browser.delete")) }
                        }
                    }
                }
            }
        }
    };
    let floating = html! {
        div class="queue" role="status" data-js="queue" hidden {
            (icon("download"))
            span class="queue__text" data-js="queue-text" {}
            svg class="bar queue__bar" viewBox="0 0 100 6" preserveAspectRatio="none" aria-hidden="true" {
                rect class="bar__track" width="100" height="6" {}
                rect class="bar__fill" width="0" height="6" data-js="queue-fill" {}
            }
            button class="btn btn--icon queue__stop" type="button" data-js="queue-stop"
                aria-label=(ctx.t("browser.queue_stop")) { (icon("x")) }
        }
    };
    app(ctx, &v.title, &v.crumbs, main, floating)
}

/// Rename, the move/copy folder picker and the name-clash question for `assets/ops.js`
/// (provisional markup until the design delivers these dialogs).
fn ops_dialogs(ctx: &Ctx) -> Markup {
    html! {
        dialog class="dialog" data-js="rename-dialog" aria-labelledby="rename-title" {
            form class="dialog__form" method="dialog" {
                h2 class="dialog__title" id="rename-title" { (ctx.t("browser.rename_title")) }
                div class="field" data-js="rename-field" {
                    label class="field__label" for="rename-name" { (ctx.t("browser.rename_label")) }
                    div class="field__control" {
                        input class="field__input" id="rename-name" type="text" required maxlength="255"
                            autocomplete="off" spellcheck="false" aria-describedby="rename-error";
                    }
                    p class="field__error" id="rename-error" data-js="rename-error" role="alert" {}
                }
                div class="dialog__actions" {
                    button class="btn" type="button" data-js="dialog-cancel" { (ctx.t("common.cancel")) }
                    button class="btn btn--primary" type="submit" value="go" { (ctx.t("browser.rename_submit")) }
                }
            }
        }
        dialog class="dialog picker" data-js="picker" aria-labelledby="picker-title" {
            form class="dialog__form" method="dialog" {
                h2 class="dialog__title" id="picker-title" data-js="picker-title" {}
                nav class="picker__path" aria-label=(ctx.t("browser.location")) data-js="picker-path" {}
                ul class="picker__list" data-js="picker-list" {}
                p class="dialog__text" data-js="picker-note" {}
                div class="dialog__actions" {
                    button class="btn" type="button" data-js="dialog-cancel" { (ctx.t("common.cancel")) }
                    button class="btn btn--primary" type="submit" value="go" data-js="picker-go" {}
                }
            }
        }
        dialog class="name-clash" data-js="op-clash" aria-labelledby="op-clash-title" {
            form class="name-clash__form" method="dialog" {
                h2 class="name-clash__title" id="op-clash-title" data-js="clash-title" {}
                p class="name-clash__existing" data-js="clash-existing" {}
                div class="name-clash__actions" {
                    button type="submit" value="keep_both" autofocus { (ctx.t("upload.keep_both")) }
                    button type="submit" value="replace" data-js="clash-replace" { (ctx.t("upload.replace")) }
                    button type="submit" value="skip" { (ctx.t("upload.skip")) }
                }
                label class="name-clash__all" data-js="clash-all-label" {
                    input type="checkbox" data-js="clash-all";
                    " " span data-js="clash-all-text" data-t=(ctx.t("upload.apply_all")) {}
                }
            }
        }
    }
}

/// One line of the shortcut overview: the keys and the i18n key of what they do.
type KeyRow = (String, &'static str);

/// The shortcut overview (`?`) for the visitor's key map.
pub fn keys_help(ctx: &Ctx) -> Markup {
    let ctrl = ctx.t("keys.ctrl");
    let k = |s: &str| s.replace("Ctrl", ctrl);
    let (list, viewer): (Vec<KeyRow>, Vec<KeyRow>) = if ctx.keys == "vim" {
        (
            vec![
                ("j / k".into(), "keys.move_cursor"),
                ("gg / G".into(), "keys.top_bottom"),
                (format!("l / {}", ctx.t("keys.enter")), "keys.open"),
                (format!("h / {}", ctx.t("keys.backspace")), "keys.up"),
                ("/".into(), "keys.filter"),
                ("n / N".into(), "keys.next_match"),
                ("Esc".into(), "keys.clear"),
                (ctx.t("keys.space").into(), "keys.toggle"),
                ("V".into(), "keys.range"),
                ("yy".into(), "keys.copy"),
                ("dd".into(), "keys.cut"),
                ("p".into(), "keys.paste"),
                ("D".into(), "keys.delete"),
                ("r".into(), "keys.rename"),
                ("o".into(), "keys.new_folder"),
                ("s".into(), "keys.share"),
                ("u".into(), "keys.upload"),
                ("z".into(), "keys.zip"),
                ("?".into(), "keys.help"),
            ],
            vec![
                ("h / l".into(), "keys.prev_next"),
                ("q".into(), "keys.close"),
            ],
        )
    } else {
        (
            vec![
                ("↑ / ↓".into(), "keys.move_cursor"),
                (
                    format!("{} / {}", ctx.t("keys.home"), ctx.t("keys.end")),
                    "keys.top_bottom",
                ),
                (ctx.t("keys.enter").into(), "keys.open"),
                ("Alt+↑".into(), "keys.up"),
                ("/".into(), "keys.filter"),
                ("Esc".into(), "keys.clear"),
                (ctx.t("keys.space").into(), "keys.toggle"),
                (format!("{}+↑ / ↓", ctx.t("keys.shift")), "keys.range"),
                (k("Ctrl+A"), "keys.select_all"),
                (k("Ctrl+C"), "keys.copy"),
                (k("Ctrl+X"), "keys.cut"),
                (k("Ctrl+V"), "keys.paste"),
                (ctx.t("keys.del").into(), "keys.delete"),
                ("F2".into(), "keys.rename"),
                ("?".into(), "keys.help"),
            ],
            vec![
                ("← / →".into(), "keys.prev_next"),
                ("Esc".into(), "keys.close"),
            ],
        )
    };
    html! {
        dialog class="dialog keys" data-js="keys-help" aria-labelledby="keys-title" {
            form class="dialog__form" method="dialog" {
                h2 class="dialog__title" id="keys-title" { (ctx.t("keys.title")) }
                p class="dialog__text" {
                    (if ctx.keys == "vim" { ctx.t("keys.intro_vim") } else { ctx.t("keys.intro_standard") })
                }
                table class="keys__table" {
                    tbody {
                        @for (key, what) in &list {
                            tr { th scope="row" { kbd { (key) } } td { (ctx.t(what)) } }
                        }
                    }
                }
                h3 class="keys__group" { (ctx.t("keys.viewer")) }
                table class="keys__table" {
                    tbody {
                        @for (key, what) in &viewer {
                            tr { th scope="row" { kbd { (key) } } td { (ctx.t(what)) } }
                        }
                    }
                }
                div class="dialog__actions" {
                    button class="btn btn--primary" type="submit" value="close" { (ctx.t("keys.close")) }
                }
            }
        }
    }
}

fn file_row(ctx: &Ctx, r: &Row) -> Markup {
    html! {
        tr class="files__row" data-type=(r.ftype) data-js="row" data-name=(r.name)
            data-dir=[r.is_dir.then_some("1")] data-size=(r.bytes)
            data-share=[r.share_href.as_deref()] data-dl=[r.download_href.as_deref()] {
            td class="files__check" {
                input type="checkbox" name="n" value=(r.name) form="sel" data-js="pick"
                    aria-label=(ctx.tf("browser.select_item", &[("name", &r.name)]));
            }
            td class="files__icon" {
                @if let Some(src) = &r.thumb {
                    img class="thumb" src=(src) alt="" width="40" height="40" loading="lazy" decoding="async";
                } @else {
                    (file_icon(r.ftype))
                }
            }
            td class="files__name" {
                a class="files__link" href=(r.href) { (r.name) }
                @if let Some(title) = &r.shared {
                    " "
                    span class="sharebadge" title=(title) {
                        (icon("share"))
                        span class="sharebadge__label" { (ctx.t("browser.shared")) }
                    }
                }
                span class="files__meta" { (r.meta) }
            }
            td class="files__size" { (r.size) }
            td class="files__date" { time datetime=(r.modified_iso) { (r.modified) } }
            td class="files__menu" {
                button class="btn btn--icon" type="button" data-js="row-menu-open" aria-haspopup="menu"
                    aria-label=(ctx.tf("browser.actions_for", &[("name", &r.name)])) { (icon("more")) }
            }
        }
    }
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
    // The sign-in page keeps the brand and gets the language switch.
    let ctx = &Ctx {
        lang_switch: Some(format!("/.cc/login?next={}", super::files::enc(&v.next))),
        ..ctx.clone()
    };
    gate(
        ctx,
        ctx.t("login.title"),
        "user",
        html! {
            form class="gate__form" method="post" action="/.cc/login" {
                input type="hidden" name="next" value=(v.next);
                (gate_field(ctx, GateField {
                    id: "login-user", name: "username", label: ctx.t("login.username"), kind: "text",
                    value: &v.username, autocomplete: "username", minlength: None, hint: None, error: None,
                }))
                (gate_field(ctx, GateField {
                    id: "login-pw", name: "password", label: ctx.t("login.password"), kind: "password",
                    value: "", autocomplete: "current-password", minlength: None, hint: None,
                    error: v.error.as_deref(),
                }))
                button class="btn btn--primary btn--block gate__submit" type="submit" {
                    span class="btn__label" { (ctx.t("login.submit")) }
                }
            }
        },
    )
}

/// One field of a gate form (`proto/share-password.html`); password fields get the
/// reveal button.
pub struct GateField<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub label: &'a str,
    /// `text` or `password`.
    pub kind: &'a str,
    pub value: &'a str,
    pub autocomplete: &'a str,
    pub minlength: Option<&'a str>,
    pub hint: Option<&'a str>,
    pub error: Option<&'a str>,
}

pub fn gate_field(ctx: &Ctx, f: GateField) -> Markup {
    let error_id = format!("{}-error", f.id);
    let hint_id = format!("{}-hint", f.id);
    let described: Vec<&str> = [
        f.hint.map(|_| hint_id.as_str()),
        f.error.map(|_| error_id.as_str()),
    ]
    .into_iter()
    .flatten()
    .collect();
    html! {
        div class="field" data-state=[f.error.map(|_| "error")] {
            label class="field__label" for=(f.id) { (f.label) }
            div class="field__control" {
                input class="field__input" id=(f.id) name=(f.name) type=(f.kind) value=(f.value)
                    autocomplete=(f.autocomplete) autocapitalize="off" spellcheck="false" required
                    minlength=[f.minlength]
                    aria-invalid=[f.error.map(|_| "true")]
                    aria-describedby=[(!described.is_empty()).then(|| described.join(" "))];
                @if f.kind == "password" {
                    button class="field__reveal btn btn--icon" type="button" aria-label=(ctx.t("common.show_password"))
                        aria-pressed="false" data-js="pw-reveal" hidden { (icon("eye")) }
                }
            }
            @if let Some(h) = f.hint {
                p class="field__hint" id=(hint_id) { (h) }
            }
            @if let Some(e) = f.error {
                p class="field__error" id=(error_id) role="alert" { (e) }
            }
        }
    }
}

pub struct PasswordView {
    /// First sign-in of an account with a one-time password: no current password asked.
    pub forced: bool,
    pub csrf: String,
    pub error: Option<String>,
    /// The error belongs to the current password (wrong, or too many tries).
    pub error_on_current: bool,
    pub saved: bool,
}

pub fn password(ctx: &Ctx, v: &PasswordView) -> Markup {
    let min = crate::auth::password::MIN_ACCOUNT_PASSWORD.to_string();
    let hint = ctx.tf("password.hint", &[("min", &min)]);
    gate(
        ctx,
        ctx.t("password.title"),
        "lock",
        html! {
            @if v.forced {
                p class="gate__text" { (ctx.t("password.forced_intro")) }
            }
            @if v.saved {
                p class="gate__text" role="status" { (ctx.t("password.saved")) }
                p { a class="btn btn--primary btn--block gate__submit" href="/" { (ctx.t("error.back_home")) } }
            } @else {
                form class="gate__form" method="post" action="/.cc/password" {
                    input type="hidden" name="csrf" value=(v.csrf);
                    @if !v.forced {
                        (gate_field(ctx, GateField {
                            id: "pw-current", name: "current", label: ctx.t("password.current"), kind: "password",
                            value: "", autocomplete: "current-password", minlength: None, hint: None,
                            error: v.error.as_deref().filter(|_| v.error_on_current),
                        }))
                    }
                    (gate_field(ctx, GateField {
                        id: "pw-new", name: "new", label: ctx.t("password.new"), kind: "password",
                        value: "", autocomplete: "new-password", minlength: Some(&min), hint: Some(&hint),
                        error: v.error.as_deref().filter(|_| !v.error_on_current),
                    }))
                    (gate_field(ctx, GateField {
                        id: "pw-repeat", name: "repeat", label: ctx.t("password.repeat"), kind: "password",
                        value: "", autocomplete: "new-password", minlength: Some(&min), hint: None,
                        error: None,
                    }))
                    button class="btn btn--primary btn--block gate__submit" type="submit" {
                        span class="btn__label" { (ctx.t("password.submit")) }
                    }
                }
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
    let ctx = &Ctx {
        scripts: {
            let mut s = ctx.scripts.clone();
            s.push("keys.js");
            s
        },
        ..ctx.clone()
    };
    layout(
        ctx,
        &v.name,
        html! {
            @if ctx.keys != "off" {
                (keys_help(ctx))
            }
            @if let Some(h) = &v.share {
                (share_head(ctx, h))
            }
            article class="viewer" data-type=(v.ftype) aria-labelledby="viewer-name" {
                header class="viewer__bar" {
                    (breadcrumb(ctx, &v.crumbs, false))
                    h1 class="viewer__name" id="viewer-name" { (v.name) }
                    nav class="viewer__nav" aria-label=(ctx.t("viewer.nav")) {
                        @if let Some(h) = &v.prev_href {
                            a class="viewer__prev" rel="prev" href=(h) data-js="viewer-prev" { (ctx.t("viewer.prev")) }
                        }
                        @if let Some(h) = &v.next_href {
                            a class="viewer__next" rel="next" href=(h) data-js="viewer-next" { (ctx.t("viewer.next")) }
                        }
                    }
                    a class="viewer__close" href=(v.close_href) data-js="viewer-close" { (ctx.t("viewer.close")) }
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
