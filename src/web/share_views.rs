//! HTML views for share links (visitor and owner side) and public uploads. Markup follows
//! the UI contract (docs/ui-handoff.md, section 5); class names are provisional until the
//! design deliverables are integrated.

use super::views::{
    Adder, Ctx, GateField, SecretField, ShareHead, UploadUi, ViewerBody, adder, confirm_dialog,
    form_field, gate, gate_field, icon, layout, row_action, secret_box, secret_result, share_head,
    tag, upload_ui, viewer_stage,
};
use maud::{Markup, html};

// ---------------------------------------------------------------------------------------
// Visitors

pub fn share_password(ctx: &Ctx, action: &str, error: Option<&str>) -> Markup {
    let ctx = &ctx.gate();
    gate(
        ctx,
        ctx.t("share.password_title"),
        "lock",
        html! {
            p class="gate__text" { (ctx.t("share.password_intro")) }
            form class="gate__form" method="post" action=(action) {
                (gate_field(ctx, GateField {
                    id: "pw", name: "password", label: ctx.t("share.password"), kind: "password",
                    value: "", autocomplete: "off", minlength: None, hint: None, error,
                }))
                button class="btn btn--primary btn--block gate__submit" type="submit" {
                    span class="btn__label" { (ctx.t("share.open")) }
                }
            }
        },
    )
}

/// `which`: not_found, expired, revoked, paused or missing.
pub fn share_unavailable(ctx: &Ctx, which: &str) -> Markup {
    let title = ctx.t("share.unavailable_title");
    let body = match which {
        "expired" => ctx.t("share.unavailable_expired"),
        "revoked" => ctx.t("share.unavailable_revoked"),
        "paused" => ctx.t("share.unavailable_paused"),
        "missing" => ctx.t("share.unavailable_missing"),
        _ => ctx.t("share.unavailable_not_found"),
    };
    layout(
        ctx,
        title,
        html! {
            section class="share-unavailable" data-state=(which) {
                h1 class="share-unavailable__title" { (title) }
                p class="share-unavailable__body" { (body) }
            }
        },
    )
}

pub struct FileCard {
    pub name: String,
    pub ftype: &'static str,
    pub size: String,
    pub modified: String,
    pub download_href: String,
    pub body: ViewerBody,
}

pub fn share_file(ctx: &Ctx, head: &ShareHead, f: &FileCard) -> Markup {
    layout(
        ctx,
        &head.name,
        html! {
            (share_head(ctx, head))
            article class="file-card" data-type=(f.ftype) aria-label=(f.name) {
                // The button first, like a folder's "Download all": in reach whatever
                // the preview's height.
                div class="file-card__bar" {
                    a class="btn btn--primary file-card__download" href=(f.download_href) download {
                        (icon("download"))
                        span class="btn__label" { (ctx.t("share.download")) }
                    }
                    p class="file-card__meta" { (f.size) " · " (f.modified) }
                }
                (viewer_stage(ctx, &f.name, f.ftype, &f.body, &f.download_href))
            }
        },
    )
}

pub fn share_dropbox(ctx: &Ctx, head: &ShareHead, upload: &UploadUi, uploads: &[String]) -> Markup {
    layout(
        ctx,
        &head.name,
        html! {
            (share_head(ctx, head))
            p class="dropbox__intro" { (ctx.t("share.dropbox_intro")) }
            (upload_ui(ctx, upload, false))
            noscript { p class="banner" data-state="warning" { (ctx.t("share.needs_js")) } }
            section class="dropbox__mine" aria-labelledby="dropbox-mine" {
                h2 id="dropbox-mine" { (ctx.t("share.your_uploads")) }
                @if uploads.is_empty() {
                    p class="dropbox__none" { (ctx.t("share.no_uploads")) }
                } @else {
                    ul class="dropbox__list" {
                        @for u in uploads.iter().rev() { li { (u) } }
                    }
                }
            }
        },
    )
}

// ---------------------------------------------------------------------------------------
// Owners

pub struct ShareRow {
    pub target: String,
    pub target_href: String,
    pub mode: String,
    pub created: String,
    pub expires: String,
    pub status_code: &'static str,
    pub status: String,
    pub last_opened: String,
    pub counts: String,
    pub owner: Option<String>,
    pub detail_href: String,
}

pub struct SharesView {
    /// Admin looking at everyone's shares.
    pub all: bool,
    /// Admin: link to switch between own and all.
    pub toggle: Option<(String, String)>,
    pub rows: Vec<ShareRow>,
}

pub fn shares_list(ctx: &Ctx, v: &SharesView) -> Markup {
    let title = if v.all {
        ctx.t("shares.all_title")
    } else {
        ctx.t("shares.title")
    };
    layout(
        ctx,
        title,
        html! {
            h1 class="shares__title" { (title) }
            @if let Some((label, href)) = &v.toggle {
                p { a class="shares__toggle" href=(href) { (label) } }
            }
            @if v.rows.is_empty() {
                p class="shares__empty" { (ctx.t("shares.empty")) }
            } @else {
                table class="shares__table" {
                    thead {
                        tr {
                            th scope="col" { (ctx.t("shares.col_target")) }
                            @if v.all { th scope="col" { (ctx.t("shares.col_owner")) } }
                            th scope="col" { (ctx.t("shares.col_mode")) }
                            th scope="col" { (ctx.t("shares.col_created")) }
                            th scope="col" { (ctx.t("shares.col_expires")) }
                            th scope="col" { (ctx.t("shares.col_status")) }
                            th scope="col" { (ctx.t("shares.col_last_opened")) }
                            th scope="col" { (ctx.t("shares.col_activity")) }
                            th scope="col" { span class="visually-hidden" { (ctx.t("shares.col_actions")) } }
                        }
                    }
                    tbody {
                        @for r in &v.rows {
                            tr class="share-row" data-state=(r.status_code) {
                                th scope="row" { a href=(r.detail_href) { (r.target) } }
                                @if let Some(o) = &r.owner { td { (o) } }
                                td { (r.mode) }
                                td { (r.created) }
                                td { (r.expires) }
                                td class="share-row__status" { (r.status) }
                                td { (r.last_opened) }
                                td { (r.counts) }
                                td { a href=(r.target_href) { (ctx.t("shares.open_target")) } }
                            }
                        }
                    }
                }
            }
        },
    )
}

pub struct ModeChoice {
    pub value: &'static str,
    pub label: String,
    pub checked: bool,
}

pub struct ExpiryChoice {
    pub value: &'static str,
    pub label: String,
    pub checked: bool,
}

pub struct NewShareView {
    pub csrf: String,
    pub vol: String,
    pub path: String,
    pub name: String,
    pub location: String,
    pub location_href: String,
    pub modes: Vec<ModeChoice>,
    pub expiries: Vec<ExpiryChoice>,
    pub passphrase: String,
    pub own: bool,
    pub note: String,
    pub max_mib: String,
    pub error: Option<String>,
}

pub fn share_new(ctx: &Ctx, v: &NewShareView) -> Markup {
    let title = ctx.t("share_create.title");
    let min = crate::shares::MIN_OWN_PASSWORD.to_string();
    layout(
        ctx,
        title,
        html! {
            h1 class="share-create__title" { (title) }
            p class="share-create__target" {
                (ctx.tf("share_create.target", &[("name", &v.name)])) " "
                a href=(v.location_href) { (v.location) }
            }
            @if let Some(e) = &v.error {
                p class="form__error" role="alert" { (e) }
            }
            form class="share-create" method="post" action="/.cc/shares/new" {
                input type="hidden" name="csrf" value=(v.csrf);
                input type="hidden" name="vol" value=(v.vol);
                input type="hidden" name="path" value=(v.path);
                input type="hidden" name="passphrase" value=(v.passphrase);
                fieldset class="share-create__modes" {
                    legend { (ctx.t("share_create.mode")) }
                    @for m in &v.modes {
                        label class="choice" {
                            input type="radio" name="mode" value=(m.value) checked[m.checked];
                            " " (m.label)
                        }
                    }
                }
                fieldset class="share-create__expiry" {
                    legend { (ctx.t("share_create.expiry")) }
                    @for e in &v.expiries {
                        label class="choice" {
                            input type="radio" name="expiry" value=(e.value) checked[e.checked];
                            " " (e.label)
                        }
                    }
                }
                fieldset class="share-create__password" {
                    legend { (ctx.t("share_create.password")) }
                    label class="choice" {
                        input type="radio" name="password_kind" value="generated" checked[!v.own];
                        " " (ctx.t("share_create.use_passphrase")) " "
                        code class="share-create__passphrase" { (v.passphrase) }
                    }
                    button class="share-create__regenerate" type="submit" name="op" value="regenerate"
                        formnovalidate { (ctx.t("share_create.regenerate")) }
                    label class="choice" {
                        input type="radio" name="password_kind" value="own" checked[v.own];
                        " " (ctx.t("share_create.use_own"))
                    }
                    label class="field" {
                        span class="field__label" { (ctx.t("share_create.own_password")) }
                        input class="field__input" type="text" name="own_password" minlength=(min)
                            autocomplete="off" spellcheck="false" aria-describedby="own-hint";
                        span class="field__hint" id="own-hint" { (ctx.tf("share_create.own_hint", &[("min", &min)])) }
                    }
                }
                label class="field" {
                    span class="field__label" { (ctx.t("share_create.note")) }
                    textarea class="field__input" name="note" rows="3"
                        maxlength=(crate::shares::MAX_NOTE_CHARS) { (v.note) }
                }
                label class="field" {
                    span class="field__label" { (ctx.t("share_create.max_size")) }
                    input class="field__input" type="number" name="max_mib" min="1" step="1"
                        value=(v.max_mib) aria-describedby="max-hint";
                    span class="field__hint" id="max-hint" { (ctx.t("share_create.max_size_hint")) }
                }
                button class="share-create__submit" type="submit" name="op" value="create" {
                    (ctx.t("share_create.submit"))
                }
            }
        },
    )
}

/// The share dialog over the browser and the viewer (review Q19; `assets/share.js`).
/// Step 1 (`data-state="create"`): the target, a summary of the defaults and "Create link";
/// "Change" opens the options (the page's fields, posted to the same URL as JSON). Step 2
/// (`data-state="created"`): "Copy as message" with its language, link and password with
/// copy buttons, the "shown once" warning, the details link and "Done". The script fills in
/// the target and the answers; without script the triggers are links to the page.
/// Provisional markup until the design delivers the dialog.
pub fn share_dialog(ctx: &Ctx, csrf: &str) -> Markup {
    let min = crate::shares::MIN_OWN_PASSWORD.to_string();
    let modes =
        crate::shares::Mode::ALL.map(|m| (m.as_str(), super::shares::mode_label(ctx, m.as_str())));
    let expiries = super::shares::expiry_choices(ctx, crate::shares::Expiry::Month);
    let first = if ctx.code() == "de" { "de" } else { "en" };
    html! {
        dialog class="dialog share-dialog" data-js="share-dialog" data-state="create"
            aria-labelledby="share-dialog-title"
            data-t-target=(ctx.t("share_create.target"))
            data-t-generated=(ctx.t("share_dialog.summary_generated"))
            data-t-own=(ctx.t("share_create.own_password"))
            data-t-copied-message=(ctx.t("share_dialog.copied_message"))
            data-t-copied-link=(ctx.t("share_dialog.copied_link"))
            data-t-copied-password=(ctx.t("share_dialog.copied_password"))
            data-t-copy-by-hand=(ctx.t("share_dialog.copy_by_hand"))
            data-t-failed=(ctx.t("ops.failed")) {
            form class="dialog__form share-dialog__step" data-step="create" method="post"
                action="/.cc/shares/new" data-js="share-form" novalidate {
                h2 class="dialog__title" id="share-dialog-title" { (ctx.t("share_create.title")) }
                p class="share-dialog__target" data-js="share-target" {}
                p class="share-dialog__summary" data-js="share-summary" {}
                p class="banner" data-state="error" role="alert" data-js="share-error-form" hidden {}
                input type="hidden" name="csrf" value=(csrf);
                input type="hidden" name="vol" data-js="share-vol";
                input type="hidden" name="path" data-js="share-path";
                input type="hidden" name="passphrase" data-js="share-passphrase-input";
                div class="share-dialog__options" id="share-options" data-js="share-options" hidden {
                    fieldset class="share-dialog__group" {
                        legend { (ctx.t("share_create.mode")) }
                        @for (value, label) in modes {
                            label class="choice" {
                                input type="radio" name="mode" value=(value) checked[value == "view"];
                                " " (label)
                            }
                        }
                    }
                    fieldset class="share-dialog__group" {
                        legend { (ctx.t("share_create.expiry")) }
                        @for e in &expiries {
                            label class="choice" {
                                input type="radio" name="expiry" value=(e.value) checked[e.checked];
                                " " (e.label)
                            }
                        }
                    }
                    fieldset class="share-dialog__group" {
                        legend { (ctx.t("share_create.password")) }
                        label class="choice" {
                            input type="radio" name="password_kind" value="generated" checked
                                aria-describedby="share-phrase";
                            " " (ctx.t("share_create.use_passphrase"))
                        }
                        div class="share-dialog__passphrase" {
                            code class="share-dialog__phrase" id="share-phrase" data-js="share-passphrase" {}
                            button class="btn btn--quiet share-dialog__regenerate" type="button" data-js="share-regenerate" {
                                (ctx.t("share_create.regenerate"))
                            }
                        }
                        label class="choice" {
                            input type="radio" name="password_kind" value="own";
                            " " (ctx.t("share_create.use_own"))
                        }
                        div class="field" data-js="share-field-own_password" hidden {
                            label class="field__label" for="share-own" { (ctx.t("share_create.own_password")) }
                            div class="field__control" {
                                input class="field__input" id="share-own" type="text" name="own_password"
                                    minlength=(min) autocomplete="off" spellcheck="false"
                                    aria-describedby="share-own-hint share-own-error";
                            }
                            p class="field__hint" id="share-own-hint" { (ctx.tf("share_create.own_hint", &[("min", &min)])) }
                            p class="field__error" id="share-own-error" data-js="share-error-own_password" {}
                        }
                    }
                    div class="field" data-js="share-field-note" {
                        label class="field__label" for="share-note" { (ctx.t("share_create.note")) }
                        div class="field__control" {
                            textarea class="field__input" id="share-note" name="note" rows="2"
                                maxlength=(crate::shares::MAX_NOTE_CHARS) aria-describedby="share-note-error" {}
                        }
                        p class="field__error" id="share-note-error" data-js="share-error-note" {}
                    }
                    div class="field" data-js="share-field-max_mib" hidden {
                        label class="field__label" for="share-max" { (ctx.t("share_create.max_size")) }
                        div class="field__control" {
                            input class="field__input" id="share-max" type="number" name="max_mib" min="1" step="1"
                                inputmode="numeric" aria-describedby="share-max-hint share-max-error";
                        }
                        p class="field__hint" id="share-max-hint" { (ctx.t("share_create.max_size_hint")) }
                        p class="field__error" id="share-max-error" data-js="share-error-max_mib" {}
                    }
                }
                div class="dialog__actions" {
                    button class="btn share-dialog__change" type="button" data-js="share-change"
                        aria-expanded="false" aria-controls="share-options" { (ctx.t("share_dialog.change")) }
                    button class="btn" type="button" data-js="dialog-cancel" { (ctx.t("common.cancel")) }
                    button class="btn btn--primary" type="submit" data-js="share-create" {
                        (icon("share"))
                        span class="btn__label" { (ctx.t("share_dialog.create")) }
                    }
                }
            }
            div class="dialog__form share-dialog__step" data-step="created" {
                h2 class="dialog__title" id="share-dialog-done-title" { (ctx.t("share_created.title")) }
                p class="banner" data-state="warning" { (ctx.t("share_created.once")) }
                div class="share-dialog__send" {
                    button class="btn btn--primary share-dialog__copy-message" type="button" data-js="share-copy-message" {
                        (icon("copy"))
                        span class="btn__label" { (ctx.t("share_dialog.copy_message")) }
                    }
                    div class="seg share-dialog__lang" role="group" aria-label=(ctx.t("share_dialog.message_lang")) {
                        @for (code, key) in [("de", "share_dialog.lang_de"), ("en", "share_dialog.lang_en")] {
                            button class="seg__item btn btn--quiet share-dialog__lang-opt" type="button" data-js="share-lang"
                                value=(code) aria-label=(ctx.t(key)) aria-pressed=(code == first)
                                aria-current=[(code == first).then_some("true")] { (code.to_uppercase()) }
                        }
                    }
                }
                textarea class="field__input share-dialog__message" data-js="share-message" readonly rows="5"
                    aria-label=(ctx.t("share_dialog.message")) hidden {}
                div class="field" {
                    label class="field__label" for="share-link" { (ctx.t("share_created.link")) }
                    div class="field__control" {
                        input class="field__input" id="share-link" type="text" readonly data-js="share-link";
                        button class="btn btn--icon btn--quiet" type="button" data-js="share-copy" data-copy="share-link"
                            data-what="link" aria-label=(ctx.t("share_created.copy_link")) title=(ctx.t("share_created.copy_link")) {
                            (icon("copy"))
                        }
                    }
                }
                div class="field" {
                    label class="field__label" for="share-password" { (ctx.t("share_created.password")) }
                    div class="field__control" {
                        input class="field__input" id="share-password" type="text" readonly data-js="share-password";
                        button class="btn btn--icon btn--quiet" type="button" data-js="share-copy" data-copy="share-password"
                            data-what="password" aria-label=(ctx.t("share_created.copy_password")) title=(ctx.t("share_created.copy_password")) {
                            (icon("copy"))
                        }
                    }
                }
                p class="share-dialog__until" {
                    (ctx.t("share_created.until")) ": " span data-js="share-until" {}
                }
                p class="share-dialog__status" role="status" data-js="share-copy-status" {}
                div class="dialog__actions" {
                    a class="share-dialog__details" href="/.cc/shares" data-js="share-details" { (ctx.t("share_created.to_details")) }
                    button class="btn" type="button" data-js="share-close" { (ctx.t("share_dialog.done")) }
                }
            }
        }
    }
}

pub struct Message {
    pub lang_name: String,
    pub code: String,
    pub text: String,
}

/// A link and password, shown once (new share, new password, public upload opened).
pub struct SecretView {
    pub title: String,
    /// The "shown once" warning: how to get a new password when this one is lost.
    pub warning: String,
    pub link: String,
    pub password: String,
    pub until: String,
    /// The creator's language first.
    pub messages: Vec<Message>,
    pub back_href: String,
    pub back_label: String,
}

/// Link and password in the shared "shown once" box (review Q26), the expiry, and the
/// ready-to-send messages (new shares only).
pub fn secret(ctx: &Ctx, v: &SecretView) -> Markup {
    let body = html! {
        (secret_box(&v.warning, &[
            SecretField {
                id: "secret-link",
                label: ctx.t("share_created.link"),
                value: &v.link,
                copy: ctx.t("share_created.copy_link"),
            },
            SecretField {
                id: "secret-password",
                label: ctx.t("share_created.password"),
                value: &v.password,
                copy: ctx.t("share_created.copy_password"),
            },
        ]))
        p class="secret-result__until" { (ctx.t("share_created.until")) ": " (v.until) }
        @for (i, m) in v.messages.iter().enumerate() {
            @let id = format!("secret-message-{}", m.code);
            @if i == 0 {
                section class="share-secret__message" {
                    h2 { (ctx.tf("share_created.message", &[("language", &m.lang_name)])) }
                    textarea class="field__input" id=(id) readonly rows="6" lang=(m.code) { (m.text) }
                    button class="btn" type="button" data-js="copy" data-copy=(id) hidden { (icon("copy")) span class="btn__label" { (ctx.t("share_created.copy_message")) } }
                }
            } @else {
                details class="share-secret__message" {
                    summary { (ctx.tf("share_created.message", &[("language", &m.lang_name)])) }
                    textarea class="field__input" id=(id) readonly rows="6" lang=(m.code) { (m.text) }
                    button class="btn" type="button" data-js="copy" data-copy=(id) hidden { (icon("copy")) span class="btn__label" { (ctx.t("share_created.copy_message")) } }
                }
            }
        }
    };
    layout(
        ctx,
        &v.title,
        secret_result(&v.title, body, (&v.back_href, &v.back_label)),
    )
}

pub struct LogRow {
    pub time: String,
    pub ip: String,
    pub country: String,
    pub action: String,
    pub file: String,
}

pub struct DetailView {
    pub id: String,
    pub csrf: String,
    pub target: String,
    pub target_href: String,
    pub link: String,
    pub owner: String,
    pub status_code: &'static str,
    pub status: String,
    pub mode: String,
    pub created: String,
    pub expires: String,
    pub note: String,
    pub counts: String,
    /// Editable while not revoked.
    pub editable: bool,
    pub modes: Vec<ModeChoice>,
    pub expiries: Vec<ExpiryChoice>,
    pub max_mib: String,
    pub log: Vec<LogRow>,
    pub error: Option<String>,
    pub saved: bool,
}

pub fn share_detail(ctx: &Ctx, v: &DetailView) -> Markup {
    let mut ctx = ctx.clone();
    ctx.scripts.push("forms.js");
    let ctx = &ctx;
    let title = ctx.tf("share_detail.title", &[("name", &v.target)]);
    let action = format!("/.cc/shares/{}", v.id);
    layout(
        ctx,
        &title,
        html! {
            h1 class="share-detail__title" { (title) }
            @if let Some(e) = &v.error {
                p class="form__error" role="alert" { (e) }
            }
            @if v.saved {
                p class="form__saved" role="status" { (ctx.t("share_detail.saved")) }
            }
            dl class="share-detail" data-state=(v.status_code) {
                dt { (ctx.t("shares.col_target")) } dd { a href=(v.target_href) { (v.target) } }
                dt { (ctx.t("share_created.link")) } dd { code { (v.link) } }
                dt { (ctx.t("shares.col_owner")) } dd { (v.owner) }
                dt { (ctx.t("shares.col_status")) } dd { (v.status) }
                dt { (ctx.t("shares.col_mode")) } dd { (v.mode) }
                dt { (ctx.t("shares.col_created")) } dd { (v.created) }
                dt { (ctx.t("shares.col_expires")) } dd { (v.expires) }
                dt { (ctx.t("shares.col_activity")) } dd { (v.counts) }
                @if !v.note.is_empty() {
                    dt { (ctx.t("share_create.note")) } dd { (v.note) }
                }
            }
            @if v.editable {
                form class="share-edit" method="post" action=(action) {
                    input type="hidden" name="csrf" value=(v.csrf);
                    input type="hidden" name="op" value="edit";
                    fieldset {
                        legend { (ctx.t("share_create.mode")) }
                        @for m in &v.modes {
                            label class="choice" {
                                input type="radio" name="mode" value=(m.value) checked[m.checked];
                                " " (m.label)
                            }
                        }
                    }
                    fieldset {
                        legend { (ctx.t("share_detail.new_expiry")) }
                        label class="choice" {
                            input type="radio" name="expiry" value="keep" checked;
                            " " (ctx.t("share_detail.keep_expiry"))
                        }
                        @for e in &v.expiries {
                            label class="choice" {
                                input type="radio" name="expiry" value=(e.value);
                                " " (e.label)
                            }
                        }
                    }
                    label class="field" {
                        span class="field__label" { (ctx.t("share_create.note")) }
                        textarea class="field__input" name="note" rows="3"
                            maxlength=(crate::shares::MAX_NOTE_CHARS) { (v.note) }
                    }
                    label class="field" {
                        span class="field__label" { (ctx.t("share_create.max_size")) }
                        input class="field__input" type="number" name="max_mib" min="1" step="1" value=(v.max_mib);
                    }
                    button type="submit" { (ctx.t("share_detail.save")) }
                }
                // Asks first with script (the old password stops working); the answer is the
                // page with the new one in the "shown once" box.
                form class="share-new-password" method="post" action=(action)
                    data-confirm=(ctx.t("share_detail.new_password_hint")) {
                    input type="hidden" name="csrf" value=(v.csrf);
                    input type="hidden" name="op" value="password";
                    button type="submit" { (ctx.t("share_detail.new_password")) }
                    p class="field__hint" { (ctx.t("share_detail.new_password_hint")) }
                }
                form class="share-revoke" method="post" action=(action) {
                    input type="hidden" name="csrf" value=(v.csrf);
                    input type="hidden" name="op" value="revoke";
                    label class="choice" {
                        input type="checkbox" name="confirm" value="1" required;
                        " " (ctx.t("share_detail.revoke_confirm"))
                    }
                    button type="submit" { (ctx.t("share_detail.revoke")) }
                }
            }
            section class="share-log" aria-labelledby="share-log-title" {
                h2 id="share-log-title" { (ctx.t("share_detail.log")) }
                @if v.log.is_empty() {
                    p { (ctx.t("share_detail.log_empty")) }
                } @else {
                    table class="share-log__table" {
                        thead {
                            tr {
                                th scope="col" { (ctx.t("share_detail.col_time")) }
                                th scope="col" { (ctx.t("share_detail.col_ip")) }
                                th scope="col" { (ctx.t("share_detail.col_country")) }
                                th scope="col" { (ctx.t("share_detail.col_action")) }
                                th scope="col" { (ctx.t("share_detail.col_file")) }
                            }
                        }
                        tbody {
                            @for r in &v.log {
                                tr { td { (r.time) } td { (r.ip) } td { (r.country) } td { (r.action) } td { (r.file) } }
                            }
                        }
                    }
                }
            }
            p { a href="/.cc/shares" { (ctx.t("shares.back")) } }
            (confirm_dialog(ctx))
        },
    )
}

// ---------------------------------------------------------------------------------------
// Public uploads

pub fn public_unlock(ctx: &Ctx, vol: &str, next: &str, open: bool, error: Option<&str>) -> Markup {
    let ctx = &ctx.gate();
    let title = ctx.tf("public_write.title", &[("volume", vol)]);
    gate(
        ctx,
        &title,
        "upload",
        html! {
            @if !open {
                p class="gate__text" data-state="closed" { (ctx.t("public_write.closed")) }
            } @else {
                p class="gate__text" { (ctx.t("public_write.intro")) }
                form class="gate__form" method="post" action="/.cc/public-write" {
                    input type="hidden" name="vol" value=(vol);
                    input type="hidden" name="next" value=(next);
                    (gate_field(ctx, GateField {
                        id: "pw", name: "password", label: ctx.t("share.password"), kind: "password",
                        value: "", autocomplete: "off", minlength: None, hint: None, error,
                    }))
                    button class="btn btn--primary btn--block gate__submit" type="submit" {
                        span class="btn__label" { (ctx.t("public_write.submit")) }
                    }
                }
            }
            p class="gate__text" { a href=(next) { (ctx.t("ops.back")) } }
        },
    )
}

pub struct VolumeRow {
    pub name: String,
    pub href: String,
    pub path: String,
    /// Compact tags: (text, long explanation).
    pub tags: Vec<(String, Option<String>)>,
    /// Public volumes: the public upload state and controls.
    pub public: Option<PublicState>,
}

pub struct PublicState {
    /// "Open until …" / "Closed".
    pub state: String,
    pub open: bool,
    pub expiries: Vec<ExpiryChoice>,
    /// The last "open" of this volume failed: why.
    pub error: Option<String>,
}

pub struct VolumesView {
    pub csrf: String,
    pub rows: Vec<VolumeRow>,
    pub error: Option<String>,
}

/// The libraries page (review Q27): one table of the volumes with their path (muted), their
/// settings as tags and, for public ones, the public upload: its state, "Open uploads"
/// (a form behind the button) and "Close uploads" (asks first).
pub fn admin_volumes(ctx: &Ctx, v: &VolumesView) -> Markup {
    let title = ctx.t("volumes.title");
    let min = crate::shares::MIN_OWN_PASSWORD.to_string();
    let action = "/.cc/admin/volumes";
    let mut ctx = ctx.clone();
    ctx.scripts.extend(["copy.js", "forms.js"]);
    let ctx = &ctx;
    layout(
        ctx,
        title,
        html! {
            h1 class="volumes__title" { (title) }
            @if let Some(e) = &v.error {
                p class="banner" data-state="error" role="alert" data-js="form-error" { (e) }
            }
            table class="admin-table volumes-table" {
                thead { tr {
                    th scope="col" { (ctx.t("users.name")) }
                    th scope="col" { (ctx.t("volumes.col_flags")) }
                    th scope="col" { (ctx.t("volumes.public_upload")) }
                } }
                tbody {
                    @for r in &v.rows {
                        @let id = format!("open-{}", r.name);
                        tr {
                            th scope="row" {
                                a class="admin-table__name" href=(r.href) { (r.name) }
                                span class="admin-path" { (r.path) }
                            }
                            td class="tags-cell" data-label=(ctx.t("volumes.col_flags")) {
                                @for (text, long) in &r.tags { (tag(text, long.as_deref(), None)) " " }
                            }
                            td class="public-upload" data-label=(ctx.t("volumes.public_upload"))
                                data-state=[r.public.as_ref().map(|p| if p.open { "open" } else { "closed" })] {
                                @if let Some(p) = &r.public {
                                    p class="public-upload__state" { (p.state) }
                                    div class="row-actions" {
                                        @let open_title = ctx.tf("volumes.open_title", &[("volume", &r.name)]);
                                        @let submit = if p.open { ctx.t("volumes.reopen") } else { ctx.t("volumes.open") };
                                        (adder(ctx, &Adder {
                                            id: &id,
                                            button: submit,
                                            plus: false,
                                            label: &open_title,
                                            title: &open_title,
                                            action,
                                            submit,
                                            error: p.error.as_deref(),
                                        }, html! {
                                            input type="hidden" name="csrf" value=(v.csrf);
                                            input type="hidden" name="vol" value=(r.name);
                                            input type="hidden" name="op" value="open";
                                            fieldset class="adder__group" {
                                                legend { (ctx.t("volumes.open_for")) }
                                                @for e in &p.expiries {
                                                    label class="choice" {
                                                        input type="radio" name="expiry" value=(e.value) checked[e.checked];
                                                        " " (e.label)
                                                    }
                                                }
                                            }
                                            (form_field(&format!("{id}-password"), ctx.t("volumes.own_password"), html! {
                                                div class="field__control" {
                                                    input class="field__input" id=(format!("{id}-password")) type="text" name="own_password"
                                                        minlength=(min) autocomplete="off" spellcheck="false";
                                                }
                                            }, Some(&ctx.tf("volumes.own_password_hint", &[("min", &min)]))))
                                            (form_field(&format!("{id}-max"), ctx.t("share_create.max_size"), html! {
                                                div class="field__control" {
                                                    input class="field__input" id=(format!("{id}-max")) type="number" name="max_mib"
                                                        min="1" step="1" inputmode="numeric";
                                                }
                                            }, None))
                                            label class="choice" {
                                                input type="checkbox" name="notify" value="1";
                                                " " (ctx.t("volumes.notify"))
                                            }
                                        }))
                                        @if p.open {
                                            (row_action(action, html! {
                                                input type="hidden" name="csrf" value=(v.csrf);
                                                input type="hidden" name="vol" value=(r.name);
                                                input type="hidden" name="op" value="close";
                                            }, ctx.t("volumes.close"), Some(&ctx.tf("volumes.close_confirm", &[("volume", &r.name)]))))
                                        }
                                    }
                                } @else {
                                    span class="admin-none" aria-hidden="true" { "–" }
                                    span class="vh" { (ctx.t("volumes.not_public")) }
                                }
                            }
                        }
                    }
                }
            }
            (confirm_dialog(ctx))
        },
    )
}
