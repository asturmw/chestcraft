//! HTML views for share links (visitor and owner side) and public uploads. Markup follows
//! the UI contract (docs/ui-handoff.md, section 5); class names are provisional until the
//! design deliverables are integrated.

use super::views::{
    Ctx, ShareHead, UploadUi, ViewerBody, layout, share_head, upload_ui, viewer_stage,
};
use maud::{Markup, html};

// ---------------------------------------------------------------------------------------
// Visitors

pub fn share_password(ctx: &Ctx, action: &str, error: Option<&str>) -> Markup {
    let title = ctx.t("share.password_title");
    layout(
        ctx,
        title,
        html! {
            h1 class="share-password__title" { (title) }
            p class="share-password__intro" { (ctx.t("share.password_intro")) }
            @if let Some(e) = error {
                p class="form__error" role="alert" { (e) }
            }
            form class="share-password" method="post" action=(action) {
                label class="field" {
                    span class="field__label" { (ctx.t("share.password")) }
                    input class="field__input" type="password" name="password" required
                        autocomplete="off" autocapitalize="off" spellcheck="false" autofocus;
                }
                button class="share-password__submit" type="submit" { (ctx.t("share.open")) }
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
    pub open_href: String,
    pub download_href: String,
    pub body: ViewerBody,
}

pub fn share_file(ctx: &Ctx, head: &ShareHead, f: &FileCard) -> Markup {
    layout(
        ctx,
        &head.name,
        html! {
            (share_head(ctx, head))
            article class="file-card" data-type=(f.ftype) {
                (viewer_stage(ctx, &f.name, f.ftype, &f.body, &f.download_href))
                h2 class="file-card__name" { a href=(f.open_href) { (f.name) } }
                p class="file-card__meta" { (f.size) " · " (f.modified) }
                a class="file-card__download" href=(f.download_href) download { (ctx.t("share.download")) }
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
            (upload_ui(ctx, upload))
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

pub struct Message {
    pub lang_name: String,
    pub code: String,
    pub text: String,
}

/// A link and password, shown once (new share, new password, public upload opened).
pub struct SecretView {
    pub title: String,
    pub link: String,
    pub password: String,
    pub until: String,
    /// The creator's language first.
    pub messages: Vec<Message>,
    pub back_href: String,
    pub back_label: String,
}

pub fn secret(ctx: &Ctx, v: &SecretView) -> Markup {
    layout(
        ctx,
        &v.title,
        html! {
            h1 class="share-secret__title" { (v.title) }
            p class="banner" data-state="warning" role="status" { (ctx.t("share_created.once")) }
            dl class="share-secret" {
                dt { (ctx.t("share_created.link")) }
                dd {
                    input class="field__input" id="secret-link" type="text" readonly value=(v.link);
                    " " button type="button" data-js="copy" data-copy="secret-link" { (ctx.t("share_created.copy_link")) }
                }
                dt { (ctx.t("share_created.password")) }
                dd {
                    input class="field__input" id="secret-password" type="text" readonly value=(v.password);
                    " " button type="button" data-js="copy" data-copy="secret-password" { (ctx.t("share_created.copy_password")) }
                }
                dt { (ctx.t("share_created.until")) }
                dd { (v.until) }
            }
            @for (i, m) in v.messages.iter().enumerate() {
                @let id = format!("secret-message-{}", m.code);
                @if i == 0 {
                    section class="share-secret__message" {
                        h2 { (ctx.tf("share_created.message", &[("language", &m.lang_name)])) }
                        textarea class="field__input" id=(id) readonly rows="6" lang=(m.code) { (m.text) }
                        button type="button" data-js="copy" data-copy=(id) { (ctx.t("share_created.copy_message")) }
                    }
                } @else {
                    details class="share-secret__message" {
                        summary { (ctx.tf("share_created.message", &[("language", &m.lang_name)])) }
                        textarea class="field__input" id=(id) readonly rows="6" lang=(m.code) { (m.text) }
                        button type="button" data-js="copy" data-copy=(id) { (ctx.t("share_created.copy_message")) }
                    }
                }
            }
            p { a href=(v.back_href) { (v.back_label) } }
        },
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
                form class="share-new-password" method="post" action=(action) {
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
        },
    )
}

// ---------------------------------------------------------------------------------------
// Public uploads

pub fn public_unlock(ctx: &Ctx, vol: &str, next: &str, open: bool, error: Option<&str>) -> Markup {
    let title = ctx.tf("public_write.title", &[("volume", vol)]);
    layout(
        ctx,
        &title,
        html! {
            h1 class="public-write__title" { (title) }
            @if !open {
                p class="banner" data-state="closed" { (ctx.t("public_write.closed")) }
            } @else {
                p class="public-write__intro" { (ctx.t("public_write.intro")) }
                @if let Some(e) = error {
                    p class="form__error" role="alert" { (e) }
                }
                form class="public-write" method="post" action="/.cc/public-write" {
                    input type="hidden" name="vol" value=(vol);
                    input type="hidden" name="next" value=(next);
                    label class="field" {
                        span class="field__label" { (ctx.t("share.password")) }
                        input class="field__input" type="password" name="password" required
                            autocomplete="off" autocapitalize="off" spellcheck="false" autofocus;
                    }
                    button type="submit" { (ctx.t("public_write.submit")) }
                }
            }
            p { a href=(next) { (ctx.t("ops.back")) } }
        },
    )
}

pub struct VolumeRow {
    pub name: String,
    pub href: String,
    pub path: String,
    pub flags: Vec<String>,
    /// Public volumes: the public upload state and controls.
    pub public: Option<PublicState>,
}

pub struct PublicState {
    /// "Open until …" / "Closed".
    pub state: String,
    pub open: bool,
    pub expiries: Vec<ExpiryChoice>,
}

pub struct VolumesView {
    pub csrf: String,
    pub rows: Vec<VolumeRow>,
    pub error: Option<String>,
}

pub fn admin_volumes(ctx: &Ctx, v: &VolumesView) -> Markup {
    let title = ctx.t("volumes.title");
    let min = crate::shares::MIN_OWN_PASSWORD.to_string();
    layout(
        ctx,
        title,
        html! {
            h1 class="volumes__title" { (title) }
            @if let Some(e) = &v.error {
                p class="form__error" role="alert" { (e) }
            }
            @for r in &v.rows {
                section class="volume-config" aria-label=(r.name) {
                    h2 { a href=(r.href) { (r.name) } }
                    p class="volume-config__path" { code { (r.path) } }
                    @if !r.flags.is_empty() {
                        ul class="volume-config__flags" { @for f in &r.flags { li { (f) } } }
                    }
                    @if let Some(p) = &r.public {
                        section class="public-upload" data-state=(if p.open { "open" } else { "closed" }) {
                            h3 { (ctx.t("volumes.public_upload")) }
                            p class="public-upload__state" { (p.state) }
                            form class="public-upload__open" method="post" action="/.cc/admin/volumes" {
                                input type="hidden" name="csrf" value=(v.csrf);
                                input type="hidden" name="vol" value=(r.name);
                                input type="hidden" name="op" value="open";
                                fieldset {
                                    legend { (ctx.t("volumes.open_for")) }
                                    @for e in &p.expiries {
                                        label class="choice" {
                                            input type="radio" name="expiry" value=(e.value) checked[e.checked];
                                            " " (e.label)
                                        }
                                    }
                                }
                                label class="field" {
                                    span class="field__label" { (ctx.t("volumes.own_password")) }
                                    input class="field__input" type="text" name="own_password" minlength=(min)
                                        autocomplete="off" spellcheck="false";
                                    span class="field__hint" { (ctx.tf("volumes.own_password_hint", &[("min", &min)])) }
                                }
                                label class="field" {
                                    span class="field__label" { (ctx.t("share_create.max_size")) }
                                    input class="field__input" type="number" name="max_mib" min="1" step="1";
                                }
                                label class="choice" {
                                    input type="checkbox" name="notify" value="1";
                                    " " (ctx.t("volumes.notify"))
                                }
                                button type="submit" {
                                    (if p.open { ctx.t("volumes.reopen") } else { ctx.t("volumes.open") })
                                }
                            }
                            @if p.open {
                                form class="public-upload__close" method="post" action="/.cc/admin/volumes" {
                                    input type="hidden" name="csrf" value=(v.csrf);
                                    input type="hidden" name="vol" value=(r.name);
                                    input type="hidden" name="op" value="close";
                                    button type="submit" { (ctx.t("volumes.close")) }
                                }
                            }
                        }
                    }
                }
            }
        },
    )
}
