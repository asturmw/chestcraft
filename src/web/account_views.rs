//! Views of the account page, the background editor and the admin's appearance page.

use super::views::{Backdrop, Ctx, backdrop, confirm_dialog, icon, layout};
use maud::{Markup, html};

/// One `<option>`.
pub struct Opt {
    pub value: String,
    pub label: String,
    pub selected: bool,
}

fn select(ctx: &Ctx, name: &str, label_key: &str, opts: &[Opt]) -> Markup {
    html! {
        label class="field" {
            span class="field__label" { (ctx.t(label_key)) }
            select class="field__input" name=(name) {
                @for o in opts {
                    option value=(o.value) selected[o.selected] { (o.label) }
                }
            }
        }
    }
}

fn status(ctx: &Ctx, error: Option<&str>, saved: bool, saved_key: &str) -> Markup {
    html! {
        @if let Some(e) = error {
            p class="form__error" role="alert" { (e) }
        }
        @if saved {
            p class="form__saved" role="status" { (ctx.t(saved_key)) }
        }
    }
}

pub struct AccountView {
    pub csrf: String,
    pub name: String,
    pub role: String,
    pub saved: bool,
    pub langs: Vec<Opt>,
    pub keys: Vec<Opt>,
    /// How folders open: automatic, list, grid.
    pub layouts: Vec<Opt>,
    /// Display name of the skin in use (the background editor edits its background).
    pub skin: String,
    /// App passwords and sessions (filled by the page handler).
    pub access: Markup,
}

/// The account page (review Q24): cards for the profile, the preferences, the look, the
/// WebDAV app passwords and the sessions. Theme, glass and skin live in the app bar's look
/// menu. With script the selects save on change (`account.js`); the button is for pages
/// without script.
pub fn account(ctx: &Ctx, v: &AccountView) -> Markup {
    let title = ctx.t("account.title");
    layout(
        ctx,
        title,
        html! {
            h1 class="account__title" { (title) }
            div class="account-cards" {
                section class="account-card account__profile" aria-labelledby="profile-title" {
                    h2 class="account-card__title" id="profile-title" { (ctx.t("account.profile")) }
                    dl class="account__identity" {
                        dt { (ctx.t("account.name")) } dd { (v.name) }
                        dt { (ctx.t("account.role")) } dd { (v.role) }
                    }
                    ul class="account-card__links" {
                        li class="account__password" {
                            a href="/.cc/password" { (icon("lock")) span { (ctx.t("account.change_password")) } }
                        }
                        // The phone's tab bar has no trash tab (review Q23): it is reached from here.
                        li class="account__trash" {
                            a href="/.cc/trash" { (icon("trash")) span { (ctx.t("nav.trash")) } }
                        }
                    }
                }
                section class="account-card prefs" aria-labelledby="prefs-title" {
                    h2 class="account-card__title" id="prefs-title" { (ctx.t("account.preferences")) }
                    form class="prefs__form" method="post" action="/.cc/account" data-js="prefs" {
                        input type="hidden" name="csrf" value=(v.csrf);
                        (select(ctx, "lang", "account.language", &v.langs))
                        (select(ctx, "keys", "account.shortcuts", &v.keys))
                        (select(ctx, "layout", "account.folder_view", &v.layouts))
                        div class="prefs__foot" {
                            button class="btn prefs__submit" type="submit" data-js="prefs-submit" { (ctx.t("account.save")) }
                            p class="form__saved" role="status" data-js="prefs-status" data-t-saved=(ctx.t("account.saved"))
                                data-t-failed=(ctx.t("ops.failed")) {
                                @if v.saved { (ctx.t("account.saved")) }
                            }
                        }
                    }
                }
                (v.access)
                section class="account-card account__look" aria-labelledby="look-title" {
                    h2 class="account-card__title" id="look-title" { (ctx.t("nav.look")) }
                    p class="account-card__text" { (ctx.t("account.look_hint")) }
                    p {
                        a href="/.cc/account/backgrounds" {
                            (icon("image"))
                            span { (ctx.tf("account.background_link", &[("skin", &v.skin)])) }
                        }
                    }
                }
            }
        },
    )
}

pub struct Tab {
    pub label: String,
    pub href: String,
    pub current: bool,
}

/// What picking a background tile sends.
pub enum TileKind {
    /// A `fill` radio: `default`, `none`, `preset:<id>` or `color`.
    Fill(String),
    /// A picture set (checkbox `sets`; several rotate). `delete`: the id of the form that
    /// deletes it (uploaded by this account).
    Set { id: String, delete: Option<String> },
}

/// One tile of the background editor: a miniature of the background and its name.
pub struct Tile {
    pub kind: TileKind,
    pub label: String,
    pub checked: bool,
    /// The miniature (no veil, no blur).
    pub backdrop: Backdrop,
    /// A gradient preset: `presets.css` colors the tile by its id.
    pub preset: Option<&'static str>,
    /// Scope of the tile's color stylesheet (`data-bg`), for color tiles.
    pub scope: Option<&'static str>,
}

/// The preview on top of the editor: the saved background with its veil and blur, under a
/// mock panel in the saved glass and opacity.
pub struct Preview {
    pub backdrop: Backdrop,
    pub glass: &'static str,
    pub opacity: Option<u8>,
}

/// May this account add a picture set?
#[derive(Clone, Copy, PartialEq)]
pub enum UploadState {
    Allowed,
    LimitReached,
    Denied,
}

pub struct BackgroundsView {
    pub csrf: String,
    pub title: String,
    /// Which background the form changes (`user:<skin>`, `default:<skin>` or `share`).
    pub target: String,
    /// The admin's targets (one per skin, the share pages); empty for an account's own,
    /// which edits the skin in use only.
    pub tabs: Vec<Tab>,
    pub preview: Preview,
    pub tiles: Vec<Tile>,
    /// Picture sets are the saved choice (the `images` radio for pages without script).
    pub images: bool,
    /// The own color is the saved choice: its fields open.
    pub color_open: bool,
    pub colors: [String; 3],
    pub stops: usize,
    pub angle: u16,
    /// Frosted and clear: (value, label, checked).
    pub glass: [(&'static str, String, bool); 2],
    /// The glass shown is the default (none stored): sent back, so saving keeps it unset.
    pub glass_auto: Option<&'static str>,
    /// Dim options, "" = automatic.
    pub dim: Vec<Opt>,
    pub blur: u8,
    /// Panel opacity options ("" = the default or the skin's own).
    pub opacity: Vec<Opt>,
    /// How often the picture sets change (an account's own background only), and whether
    /// it shows at load (two or more sets ticked).
    pub rotation: Option<(Vec<Opt>, bool)>,
    pub count: String,
    pub upload: UploadState,
    pub limit_text: Option<String>,
    pub error: Option<String>,
    /// The error belongs to the upload: it shows in the upload form, which opens.
    pub upload_error: Option<String>,
    pub saved: bool,
    /// Link back (the appearance page for the admin's defaults).
    pub back: Option<(String, String)>,
    /// Stylesheets the page links: preset colors, the scoped colors of tiles and preview.
    pub styles: Vec<String>,
}

fn steps(ctx: &Ctx, name: &str, label_key: &str, current: u8) -> Markup {
    let opts: Vec<Opt> = (0..=crate::appearance::MAX_STEP)
        .map(|n| Opt {
            value: n.to_string(),
            label: ctx.t(&format!("backgrounds.step_{n}")).to_string(),
            selected: n == current,
        })
        .collect();
    select(ctx, name, label_key, &opts)
}

fn tile(ctx: &Ctx, t: &Tile) -> Markup {
    let (kind, name, value, delete) = match &t.kind {
        TileKind::Fill(v) => ("radio", "fill", v.as_str(), None),
        TileKind::Set { id, delete } => ("checkbox", "sets", id.as_str(), delete.as_deref()),
    };
    html! {
        div class="bg-tile" data-kind=(kind) data-preset=[t.preset] data-bg=[t.scope] {
            label class="bg-tile__pick" {
                input class="bg-tile__input" type=(kind) name=(name) value=(value) checked[t.checked] data-js="bg-pick";
                span class="bg-tile__swatch" data-js="bg-swatch" { (backdrop(&t.backdrop, ctx.theme)) }
                span class="bg-tile__name" { (t.label) }
            }
            @if let Some(form) = delete {
                button class="btn btn--icon btn--quiet bg-tile__delete" type="submit" form=(form)
                    aria-label=(ctx.tf("backgrounds.delete_named", &[("name", &t.label)])) title=(ctx.t("backgrounds.delete")) {
                    (icon("trash"))
                }
            }
        }
    }
}

/// The upload tile: the form behind it (its fields belong to the form `bg-upload`, outside
/// the editor's form). Without script a `<details>` that opens in the grid; with it
/// (`account.js`) the fields open in a dialog.
fn upload_tile(ctx: &Ctx, v: &BackgroundsView) -> Markup {
    html! {
        @match v.upload {
            UploadState::Allowed => {
                details class="bg-tile bg-tile--upload" data-js="bg-upload" open[v.upload_error.is_some()] {
                    summary class="bg-tile__pick" {
                        span class="bg-tile__swatch bg-tile__swatch--upload" { (icon("upload")) }
                        span class="bg-tile__name" { (ctx.t("backgrounds.upload_tile")) }
                        span class="bg-tile__note" { (v.count) }
                    }
                    div class="bg-upload" data-js="bg-upload-body" {
                        h3 class="dialog__title bg-upload__title" id="bg-upload-title" { (ctx.t("backgrounds.upload_title")) }
                        @if let Some(e) = &v.upload_error {
                            p class="banner" data-state="error" role="alert" { (e) }
                        }
                        p class="field__hint" id="bg-upload-hint" { (ctx.t("backgrounds.upload_hint")) }
                        @for slot in crate::appearance::SLOTS {
                            label class="field" {
                                span class="field__label" { (ctx.t(&format!("backgrounds.slot_{slot}"))) }
                                input class="field__input" type="file" name=(slot) accept="image/*" form="bg-upload"
                                    aria-describedby="bg-upload-hint";
                            }
                        }
                        p class="bg-upload__count" { (v.count) }
                        div class="dialog__actions" {
                            button class="btn" type="button" data-js="dialog-cancel" hidden { (ctx.t("common.cancel")) }
                            button class="btn btn--primary" type="submit" form="bg-upload" { (ctx.t("backgrounds.upload")) }
                        }
                    }
                }
            }
            state => {
                div class="bg-tile bg-tile--upload" data-state="off" {
                    div class="bg-tile__pick" {
                        span class="bg-tile__swatch bg-tile__swatch--upload" { (icon("upload")) }
                        span class="bg-tile__name" { (ctx.t("backgrounds.upload_tile")) }
                        span class="bg-tile__note" {
                            @if state == UploadState::Denied {
                                (ctx.t("backgrounds.upload_denied"))
                            } @else {
                                (v.limit_text.as_deref().unwrap_or_default())
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The background editor (review Q9, Q32): the preview on top with the glass switch, the
/// background tiles (default, the skin's own, presets, own color, picture sets, upload),
/// the rotation once two or more sets are ticked, the own color's fields, then "Fine-tune"
/// (veil, blur, panel opacity). One save button; with script (`account.js`) the preview
/// follows every choice before it is saved.
pub fn backgrounds(ctx: &Ctx, v: &BackgroundsView) -> Markup {
    let mut ctx = ctx.clone();
    ctx.styles.extend(v.styles.iter().cloned());
    ctx.scripts.extend(["forms.js", "account.js"]);
    let ctx = &ctx;
    let (fills, sets): (Vec<&Tile>, Vec<&Tile>) = v
        .tiles
        .iter()
        .partition(|t| matches!(t.kind, TileKind::Fill(_)));
    layout(
        ctx,
        &v.title,
        html! {
            @if let Some((label, href)) = &v.back {
                p class="backgrounds__back" { a href=(href) { (label) } }
            }
            h1 class="backgrounds__title" { (v.title) }
            @if !v.tabs.is_empty() {
                nav class="tabs" aria-label=(ctx.t("backgrounds.skins")) {
                    @for t in &v.tabs {
                        @if t.current {
                            span class="tabs__item" aria-current="page" { (t.label) }
                        } @else {
                            a class="tabs__item" href=(t.href) { (t.label) }
                        }
                    }
                }
            }
            (status(ctx, v.error.as_deref(), v.saved, "backgrounds.saved"))
            form class="bg-editor" method="post" action="/.cc/account/backgrounds" data-js="bg-editor" {
                input type="hidden" name="csrf" value=(v.csrf);
                input type="hidden" name="target" value=(v.target);
                div class="bg-editor__top" {
                    div class="bg-preview" data-js="bg-preview" data-bg="preview" data-glass=(v.preview.glass)
                        data-opacity=[v.preview.opacity] {
                        (backdrop(&v.preview.backdrop, ctx.theme))
                        div class="bg-preview__window" aria-hidden="true" {
                            span class="bg-preview__bar" {}
                            span class="bg-preview__side" {}
                            span class="bg-preview__panel" {
                                span class="bg-preview__line" {}
                                span class="bg-preview__line" {}
                                span class="bg-preview__line" {}
                            }
                        }
                        span class="vh" { (ctx.t("backgrounds.preview")) }
                    }
                    fieldset class="bg-editor__glass" {
                        legend class="bg-editor__label" { (ctx.t("backgrounds.glass")) }
                        div class="seg bg-glass" {
                            @for (value, label, on) in &v.glass {
                                label class="seg__item btn btn--quiet bg-glass__item" {
                                    input class="vh" type="radio" name="glass" value=(value) checked[*on] data-js="bg-glass";
                                    (label)
                                }
                            }
                        }
                        @if let Some(auto) = v.glass_auto {
                            input type="hidden" name="glass_auto" value=(auto);
                        }
                    }
                }
                fieldset class="bg-tiles" {
                    legend class="bg-editor__label" { (ctx.t("backgrounds.fill")) }
                    div class="bg-tiles__grid" {
                        @for t in &fills { (tile(ctx, t)) }
                        label class="bg-tiles__images" data-js="bg-images" {
                            input type="radio" name="fill" value="images" checked[v.images] data-js="bg-pick";
                            " " (ctx.t("backgrounds.fill_images"))
                        }
                        @for t in &sets { (tile(ctx, t)) }
                        (upload_tile(ctx, v))
                    }
                    p class="field__hint" { (ctx.t("backgrounds.pictures_hint")) }
                }
                @if let Some((opts, shown)) = &v.rotation {
                    div class="bg-editor__rotation" data-js="bg-rotation" hidden[!shown] {
                        (select(ctx, "rotation", "account.rotation", opts))
                    }
                }
                details class="bg-editor__more bg-editor__color" data-js="bg-colors" open[v.color_open] {
                    summary { (ctx.t("backgrounds.color_fields")) }
                    label class="field" {
                        span class="field__label" { (ctx.t("backgrounds.stops")) }
                        select class="field__input" name="stops" data-js="bg-stops" {
                            @for n in 1..=3usize {
                                option value=(n) selected[n == v.stops] { (n) }
                            }
                        }
                    }
                    div class="bg-colors" {
                        @for (i, c) in v.colors.iter().enumerate() {
                            label class="field" data-js="bg-color" {
                                span class="field__label" { (ctx.tf("backgrounds.color_n", &[("n", &(i + 1).to_string())])) }
                                input class="field__input" type="color" name=(format!("c{}", i + 1)) value=(c);
                            }
                        }
                    }
                    label class="field" {
                        span class="field__label" { (ctx.t("backgrounds.angle")) }
                        input class="field__input" type="number" name="angle" min="0" max="359" step="1" value=(v.angle);
                    }
                }
                details class="bg-editor__more bg-editor__fine" {
                    summary { (ctx.t("backgrounds.fine_tune")) }
                    (select(ctx, "dim", "backgrounds.dim", &v.dim))
                    (steps(ctx, "blur", "backgrounds.blur", v.blur))
                    label class="field" {
                        span class="field__label" { (ctx.t("backgrounds.opacity")) }
                        select class="field__input" name="opacity" aria-describedby="bg-opacity-hint" {
                            @for o in &v.opacity {
                                option value=(o.value) selected[o.selected] { (o.label) }
                            }
                        }
                        span class="field__hint" id="bg-opacity-hint" { (ctx.t("backgrounds.opacity_hint")) }
                    }
                }
                div class="bg-editor__foot" {
                    button class="btn btn--primary bg-editor__submit" type="submit" { (ctx.t("backgrounds.save")) }
                }
            }
            @for t in &sets {
                @if let TileKind::Set { id, delete: Some(form) } = &t.kind {
                    form id=(form) method="post" action="/.cc/account/backgrounds"
                        data-confirm=(ctx.tf("backgrounds.delete_confirm", &[("name", &t.label)])) {
                        input type="hidden" name="csrf" value=(v.csrf);
                        input type="hidden" name="target" value=(v.target);
                        input type="hidden" name="delete" value=(id);
                    }
                }
            }
            @if v.upload == UploadState::Allowed {
                form id="bg-upload" method="post" action="/.cc/account/backgrounds/upload" enctype="multipart/form-data" {
                    input type="hidden" name="csrf" value=(v.csrf);
                    input type="hidden" name="target" value=(v.target);
                }
                dialog class="dialog bg-upload-dialog" data-js="bg-upload-dialog" aria-labelledby="bg-upload-title" {}
            }
            (confirm_dialog(ctx))
        },
    )
}

pub struct BackgroundRow {
    pub label: String,
    pub desc: String,
    pub href: String,
}

pub struct AppearanceView {
    pub csrf: String,
    pub saved: bool,
    /// Empty when there is nothing to choose.
    pub default_skins: Vec<Opt>,
    pub share_skins: Vec<Opt>,
    pub rotations: Vec<Opt>,
    pub backgrounds: Vec<BackgroundRow>,
    pub limit: String,
    pub denied: String,
}

pub fn appearance(ctx: &Ctx, v: &AppearanceView) -> Markup {
    let title = ctx.t("appearance.title");
    layout(
        ctx,
        title,
        html! {
            h1 class="appearance__title" { (title) }
            (status(ctx, None, v.saved, "appearance.saved"))
            form class="appearance__form" method="post" action="/.cc/admin/appearance" {
                input type="hidden" name="csrf" value=(v.csrf);
                @if !v.default_skins.is_empty() {
                    (select(ctx, "default_skin", "appearance.default_skin", &v.default_skins))
                    (select(ctx, "share_skin", "appearance.share_skin", &v.share_skins))
                }
                (select(ctx, "rotation", "appearance.rotation", &v.rotations))
                button type="submit" { (ctx.t("appearance.save")) }
            }
            section class="appearance__backgrounds" {
                h2 { (ctx.t("appearance.backgrounds")) }
                table class="appearance__table" {
                    tbody {
                        @for r in &v.backgrounds {
                            tr {
                                th scope="row" { (r.label) }
                                td { (r.desc) }
                                td { a href=(r.href) { (ctx.t("appearance.edit")) } }
                            }
                        }
                    }
                }
            }
            section class="appearance__limits" {
                h2 { (ctx.t("appearance.uploads")) }
                p { (v.limit) }
                p { (v.denied) }
            }
        },
    )
}
