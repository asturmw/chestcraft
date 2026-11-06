//! Views of the account page, the background editor and the admin's appearance page.

use super::views::{Ctx, layout};
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
    pub themes: Vec<Opt>,
    /// Empty when there is nothing to choose.
    pub skins: Vec<Opt>,
    pub keys: Vec<Opt>,
    pub rotations: Vec<Opt>,
    pub admin: bool,
}

pub fn account(ctx: &Ctx, v: &AccountView) -> Markup {
    let title = ctx.t("account.title");
    layout(
        ctx,
        title,
        html! {
            h1 class="account__title" { (title) }
            dl class="account__identity" {
                dt { (ctx.t("account.name")) } dd { (v.name) }
                dt { (ctx.t("account.role")) } dd { (v.role) }
            }
            p class="account__password" { a href="/.cc/password" { (ctx.t("account.change_password")) } }
            section class="prefs" aria-labelledby="prefs-title" {
                h2 id="prefs-title" { (ctx.t("account.preferences")) }
                (status(ctx, None, v.saved, "account.saved"))
                form class="prefs__form" method="post" action="/.cc/account" {
                    input type="hidden" name="csrf" value=(v.csrf);
                    (select(ctx, "lang", "account.language", &v.langs))
                    (select(ctx, "theme", "account.theme", &v.themes))
                    @if !v.skins.is_empty() {
                        (select(ctx, "skin", "account.skin", &v.skins))
                    }
                    (select(ctx, "keys", "account.shortcuts", &v.keys))
                    (select(ctx, "rotation", "account.rotation", &v.rotations))
                    button class="prefs__submit" type="submit" { (ctx.t("account.save")) }
                }
            }
            section class="account__backgrounds" {
                h2 { (ctx.t("account.backgrounds")) }
                p { a href="/.cc/account/backgrounds" { (ctx.t("account.backgrounds_link")) } }
                @if v.admin {
                    p { a href="/.cc/admin/appearance" { (ctx.t("account.appearance_link")) } }
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

pub struct SetCard {
    pub id: String,
    pub selected: bool,
    /// Uploaded by this account.
    pub deletable: bool,
    /// Slot label and preview URL.
    pub pictures: Vec<(String, String)>,
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
    pub tabs: Vec<Tab>,
    /// `default`, `none`, `color` or `images`.
    pub fill: &'static str,
    /// Offer "use the default" (own backgrounds only).
    pub allow_default: bool,
    pub colors: [String; 3],
    pub stops: usize,
    pub angle: u16,
    pub dim: u8,
    pub blur: u8,
    pub sets: Vec<SetCard>,
    pub count: String,
    pub upload: UploadState,
    pub limit_text: Option<String>,
    pub error: Option<String>,
    pub saved: bool,
    /// Link back (the appearance page for the admin's defaults).
    pub back: Option<(String, String)>,
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

pub fn backgrounds(ctx: &Ctx, v: &BackgroundsView) -> Markup {
    let fills: &[(&str, &str)] = &[
        ("default", "backgrounds.fill_default"),
        ("none", "backgrounds.fill_none"),
        ("color", "backgrounds.fill_color"),
        ("images", "backgrounds.fill_images"),
    ];
    layout(
        ctx,
        &v.title,
        html! {
            @if let Some((label, href)) = &v.back {
                p class="backgrounds__back" { a href=(href) { (label) } }
            }
            h1 class="backgrounds__title" { (v.title) }
            @if v.tabs.len() > 1 {
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
            form class="bg-editor" method="post" action="/.cc/account/backgrounds" {
                input type="hidden" name="csrf" value=(v.csrf);
                input type="hidden" name="target" value=(v.target);
                fieldset class="bg-editor__fill" {
                    legend { (ctx.t("backgrounds.fill")) }
                    @for (value, key) in fills {
                        @if *value != "default" || v.allow_default {
                            label class="choice" {
                                input type="radio" name="fill" value=(value) checked[v.fill == *value];
                                " " (ctx.t(key))
                            }
                        }
                    }
                }
                fieldset class="bg-editor__color" {
                    legend { (ctx.t("backgrounds.fill_color")) }
                    label class="field" {
                        span class="field__label" { (ctx.t("backgrounds.stops")) }
                        select class="field__input" name="stops" {
                            @for n in 1..=3usize {
                                option value=(n) selected[n == v.stops] { (n) }
                            }
                        }
                    }
                    @for (i, c) in v.colors.iter().enumerate() {
                        label class="field" {
                            span class="field__label" { (ctx.tf("backgrounds.color_n", &[("n", &(i + 1).to_string())])) }
                            input class="field__input" type="color" name=(format!("c{}", i + 1)) value=(c);
                        }
                    }
                    label class="field" {
                        span class="field__label" { (ctx.t("backgrounds.angle")) }
                        input class="field__input" type="number" name="angle" min="0" max="359" step="1" value=(v.angle);
                    }
                }
                fieldset class="bg-editor__images" {
                    legend { (ctx.t("backgrounds.pictures")) }
                    @if v.sets.is_empty() {
                        p class="bg-editor__empty" { (ctx.t("backgrounds.none_uploaded")) }
                    } @else {
                        p class="field__hint" { (ctx.t("backgrounds.pictures_hint")) }
                        ul class="bg-sets" {
                            @for (n, s) in v.sets.iter().enumerate() {
                                li class="bg-set" data-state=(if s.selected { "selected" } else { "idle" }) {
                                    label class="choice bg-set__pick" {
                                        input type="checkbox" name="sets" value=(s.id) checked[s.selected];
                                        " " (ctx.tf("backgrounds.use_n", &[("n", &(n + 1).to_string())]))
                                    }
                                    @for (label, url) in &s.pictures {
                                        figure class="bg-set__picture" {
                                            img src=(url) alt="" loading="lazy";
                                            figcaption { (label) }
                                        }
                                    }
                                    @if s.deletable {
                                        button class="bg-set__delete" type="submit" form="bg-delete"
                                            name="delete" value=(s.id) { (ctx.t("backgrounds.delete")) }
                                    }
                                }
                            }
                        }
                    }
                }
                (steps(ctx, "dim", "backgrounds.dim", v.dim))
                (steps(ctx, "blur", "backgrounds.blur", v.blur))
                button class="bg-editor__submit" type="submit" { (ctx.t("backgrounds.save")) }
            }
            form id="bg-delete" method="post" action="/.cc/account/backgrounds" {
                input type="hidden" name="csrf" value=(v.csrf);
                input type="hidden" name="target" value=(v.target);
            }
            section class="bg-upload" data-state=(match v.upload {
                UploadState::Allowed => "allowed",
                UploadState::LimitReached => "limit",
                UploadState::Denied => "denied",
            }) {
                h2 { (ctx.t("backgrounds.upload_title")) }
                p class="bg-upload__count" { (v.count) }
                @match v.upload {
                    UploadState::Denied => p { (ctx.t("backgrounds.upload_denied")) },
                    UploadState::LimitReached => p { (v.limit_text.as_deref().unwrap_or_default()) },
                    UploadState::Allowed => form class="bg-upload__form" method="post"
                        action="/.cc/account/backgrounds/upload" enctype="multipart/form-data" {
                        input type="hidden" name="csrf" value=(v.csrf);
                        input type="hidden" name="target" value=(v.target);
                        p class="field__hint" id="bg-upload-hint" { (ctx.t("backgrounds.upload_hint")) }
                        @for slot in crate::appearance::SLOTS {
                            label class="field" {
                                span class="field__label" { (ctx.t(&format!("backgrounds.slot_{slot}"))) }
                                input class="field__input" type="file" name=(slot) accept="image/*"
                                    aria-describedby="bg-upload-hint";
                            }
                        }
                        button type="submit" { (ctx.t("backgrounds.upload")) }
                    },
                }
            }
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
