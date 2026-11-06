//! The account page (language, theme, skin, shortcuts, background rotation), the
//! background editor (an account's own background per skin, and the admin's defaults),
//! the admin's appearance page, and the files pages load for their backdrop.
//!
//! Background pictures are visible to their owner, and to everyone once the admin uses
//! them as a default (share pages show them to visitors without an account).

use super::account_views::{
    self as av, AccountView, AppearanceView, BackgroundRow, BackgroundsView, Opt, SetCard, Tab,
    UploadState,
};
use super::auth::{audit, csrf_ok, forbidden, same_origin};
use super::files::{Fail, enc, fail};
use super::query::Query;
use super::views::{Backdrop, Ctx};
use super::{AppState, ClientInfo, Current, CurrentUser, Shared, page_ctx};
use crate::appearance::{self as look, AddError, DEFAULT_DIM, DEFAULT_ROTATION, MAX_STEP, Seed};
use crate::auth::perm::{ADMIN_ROLE, Principal};
use crate::db::{Appearance, Background, Fill};
use axum::extract::{Extension, Multipart, Path, State};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Redirect, Response};
use std::collections::BTreeMap;

const LOGIN_FOR_ACCOUNT: &str = "/.cc/login?next=%2F.cc%2Faccount";

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    tokio::task::spawn_blocking(f).await.ok()
}

/// Skins to choose from: the embedded ones, or the fallback name when there are none.
fn skin_names(st: &AppState) -> Vec<String> {
    if st.skins.is_empty() {
        vec![look::FALLBACK_SKIN.to_string()]
    } else {
        st.skins.clone()
    }
}

/// Display name of a skin: `skin.<name>` from the language file, else the name.
fn skin_label(ctx: &Ctx, skin: &str) -> String {
    let key = format!("skin.{skin}");
    if ctx.i18n.has(ctx.lang, &key) {
        return ctx.tf(&key, &[]);
    }
    let mut c = skin.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

fn rotation_label(ctx: &Ctx, mode: &str) -> String {
    ctx.tf(&format!("rotation.{mode}"), &[])
}

// ---------------------------------------------------------------------------------------
// What a page shows

/// Skin and backdrop of a page: the account's choices over the admin's defaults; the
/// admin's share skin and background for visitors without an account.
pub fn look(st: &AppState, current: Option<&CurrentUser>) -> (String, Backdrop) {
    let a = st.looks.appearance();
    let default_skin = look::pick_skin(&st.skins, a.default_skin.as_deref(), None);
    let day = look::day_number(jiff::Timestamp::now(), &st.cfg.server.timezone);
    let default_mode = a
        .rotation
        .as_deref()
        .filter(|m| look::is_rotation(m))
        .unwrap_or(DEFAULT_ROTATION);
    match current {
        None => {
            let skin = look::pick_skin(&st.skins, a.share_skin.as_deref(), Some(&default_skin));
            let backdrop = a
                .share_background
                .as_ref()
                .and_then(|bg| backdrop(st, bg, None, default_mode, Seed::Anonymous, day));
            (skin, backdrop.unwrap_or_default())
        }
        Some(c) => {
            let skin = look::pick_skin(&st.skins, c.settings.skin.as_deref(), Some(&default_skin));
            let mode = c
                .settings
                .rotation
                .as_deref()
                .filter(|m| look::is_rotation(m))
                .unwrap_or(default_mode);
            let seed = Seed::Session(&c.session_hash);
            let backdrop = c
                .settings
                .backgrounds
                .get(&skin)
                .and_then(|bg| backdrop(st, bg, Some(&c.key), mode, seed, day))
                .or_else(|| {
                    a.backgrounds
                        .get(&skin)
                        .and_then(|bg| backdrop(st, bg, None, mode, seed, day))
                });
            (skin, backdrop.unwrap_or_default())
        }
    }
}

/// The backdrop of one background. None when its picture sets are all gone (or, for an
/// account's own background, not its own), so the default applies.
fn backdrop(
    st: &AppState,
    bg: &Background,
    owner: Option<&str>,
    mode: &str,
    seed: Seed,
    day: i64,
) -> Option<Backdrop> {
    let (dim, blur) = (bg.dim.min(MAX_STEP), bg.blur.min(MAX_STEP));
    match &bg.fill {
        Fill::None => Some(Backdrop {
            state: "none",
            dim,
            blur,
            ..Default::default()
        }),
        Fill::Color { colors, angle } => Some(Backdrop {
            state: "color",
            dim,
            blur,
            pictures: None,
            stylesheet: Some(format!(
                "/.cc/backdrop.css?{}",
                look::color_query(colors, *angle)
            )),
        }),
        Fill::Images { sets } => {
            let usable: Vec<_> = sets
                .iter()
                .filter_map(|id| st.looks.set(id))
                .filter(|s| owner.is_none_or(|o| s.owner == o) && !s.images.is_empty())
                .collect();
            if usable.is_empty() {
                return None;
            }
            let set = &usable[look::pick(usable.len(), mode, seed, day)];
            let url = |shape: &str| {
                look::slot_for(set, shape).map(|s| format!("/.cc/bg/{}/{s}.jpg", set.id))
            };
            Some(Backdrop {
                state: "image",
                dim,
                blur,
                pictures: Some([url("wide")?, url("square")?, url("tall")?]),
                stylesheet: None,
            })
        }
    }
}

/// `/.cc/bg/<set>/<slot>.jpg` (or `.small.jpg`): one background picture.
pub async fn picture(
    State(st): State<Shared>,
    Extension(current): Extension<Current>,
    Path((id, file)): Path<(String, String)>,
) -> Response {
    let (slot, small) = match file.strip_suffix(".small.jpg") {
        Some(s) => (s, true),
        None => match file.strip_suffix(".jpg") {
            Some(s) => (s, false),
            None => return StatusCode::NOT_FOUND.into_response(),
        },
    };
    let Some(set) = st.looks.set(&id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mine = current.as_ref().is_some_and(|c| c.key == set.owner);
    if !look::is_slot(slot)
        || !set.images.iter().any(|i| i.slot == slot)
        || !(mine || st.looks.is_public(&id))
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    match tokio::fs::read(st.looks.file(&id, slot, small)).await {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "image/jpeg"),
                // A set never changes (a new upload is a new set), and `private` keeps it
                // out of shared caches.
                (
                    header::CACHE_CONTROL,
                    "private, max-age=31536000, immutable",
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(e) => {
            tracing::warn!(set = id, slot, "background picture unreadable: {e}");
            StatusCode::NOT_FOUND.into_response()
        }
    }
}

/// `/.cc/backdrop.css?c=1a2b3c-445566&a=135`: the backdrop color as a stylesheet (the page
/// policy forbids inline styles). Depends on the URL only.
pub async fn backdrop_css(uri: Uri) -> Response {
    let q = Query::parse(uri.query());
    match look::color_css(q.get("c").unwrap_or(""), q.get("a").unwrap_or("")) {
        Some(css) => (
            [
                (header::CONTENT_TYPE, "text/css; charset=utf-8"),
                (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
            ],
            css,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

// ---------------------------------------------------------------------------------------
// Account page

fn opt(value: &str, label: impl Into<String>, selected: bool) -> Opt {
    Opt {
        value: value.to_string(),
        label: label.into(),
        selected,
    }
}

fn account_view(st: &AppState, ctx: &Ctx, cu: &CurrentUser, saved: bool) -> AccountView {
    let s = &cu.settings;
    let a = st.looks.appearance();
    let lang = s.lang.as_deref().filter(|c| st.i18n.by_code(c).is_some());
    let mut langs = vec![opt("", ctx.t("account.language_auto"), lang.is_none())];
    langs.extend(
        st.i18n
            .languages()
            .map(|(_, code, name)| opt(code, name, lang == Some(code))),
    );
    let theme = s.theme.as_deref().unwrap_or("system");
    let themes = ["system", "dark", "light"]
        .map(|t| opt(t, ctx.tf(&format!("account.theme_{t}"), &[]), theme == t))
        .into();
    let skins = if st.skins.len() > 1 {
        let default_skin = look::pick_skin(&st.skins, a.default_skin.as_deref(), None);
        let own = s
            .skin
            .as_deref()
            .filter(|k| st.skins.iter().any(|x| x == k));
        let mut v = vec![opt(
            "",
            ctx.tf(
                "account.skin_default",
                &[("skin", &skin_label(ctx, &default_skin))],
            ),
            own.is_none(),
        )];
        v.extend(
            st.skins
                .iter()
                .map(|k| opt(k, skin_label(ctx, k), own == Some(k.as_str()))),
        );
        v
    } else {
        Vec::new()
    };
    let keys_now = s.keys.as_deref().unwrap_or("standard");
    let keys = ["standard", "vim", "off"]
        .map(|k| {
            opt(
                k,
                ctx.tf(&format!("account.shortcuts_{k}"), &[]),
                keys_now == k,
            )
        })
        .into();
    let default_mode = a.rotation.as_deref().unwrap_or(DEFAULT_ROTATION);
    let mode = s.rotation.as_deref().filter(|m| look::is_rotation(m));
    let mut rotations = vec![opt(
        "",
        ctx.tf(
            "account.rotation_default",
            &[("mode", &rotation_label(ctx, default_mode))],
        ),
        mode.is_none(),
    )];
    rotations.extend(
        look::ROTATIONS
            .iter()
            .map(|m| opt(m, rotation_label(ctx, m), mode == Some(*m))),
    );
    AccountView {
        csrf: cu.csrf.clone(),
        name: cu.name.clone(),
        role: cu.role.clone(),
        saved,
        langs,
        themes,
        skins,
        keys,
        rotations,
        admin: cu.role == ADMIN_ROLE,
    }
}

pub async fn page(
    State(st): State<Shared>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let Some(cu) = current.clone() else {
        return Redirect::to(LOGIN_FOR_ACCOUNT).into_response();
    };
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    let saved = Query::parse(uri.query()).has("saved");
    av::account(&ctx, &account_view(&st, &ctx, &cu, saved)).into_response()
}

/// A preference field: absent = keep, `Some(None)` = back to the default.
type Change = Option<Option<String>>;

/// Check one field with `valid`; `reset` values clear it. Err: a value nobody offered.
fn change(
    form: &Query,
    key: &str,
    reset: &[&str],
    valid: impl Fn(&str) -> bool,
) -> Result<Change, ()> {
    match form.get(key) {
        None => Ok(None),
        Some(v) if reset.contains(&v) => Ok(Some(None)),
        Some(v) if valid(v) => Ok(Some(Some(v.to_string()))),
        Some(_) => Err(()),
    }
}

/// POST `/.cc/account`: save preferences. Every field is optional, so a quick switch can
/// send just one (with `next` to return to its page).
pub async fn save_prefs(
    State(st): State<Shared>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let Some(cu) = current.clone() else {
        return Redirect::to(LOGIN_FOR_ACCOUNT).into_response();
    };
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    let form = Query::parse(Some(&body));
    if !csrf_ok(&headers, &st, &cu, form.get("csrf").unwrap_or("")) {
        return forbidden(&ctx);
    }
    let fields = (|| {
        Ok::<_, ()>((
            change(&form, "lang", &[""], |c| st.i18n.by_code(c).is_some())?,
            change(&form, "theme", &["", "system"], |t| {
                matches!(t, "dark" | "light")
            })?,
            change(&form, "skin", &[""], |k| st.skins.iter().any(|x| x == k))?,
            change(&form, "keys", &[], |k| {
                matches!(k, "standard" | "vim" | "off")
            })?,
            change(&form, "rotation", &[""], look::is_rotation)?,
        ))
    })();
    let Ok((lang, theme, skin, keys, rotation)) = fields else {
        return fail(&ctx, Fail::BadRequest);
    };
    let (st2, key) = (st.clone(), cu.key.clone());
    let saved = blocking(move || {
        st2.db.update_user(&key, |u| {
            let s = &mut u.settings;
            for (slot, value) in [
                (&mut s.lang, lang),
                (&mut s.theme, theme),
                (&mut s.skin, skin),
                (&mut s.keys, keys),
                (&mut s.rotation, rotation),
            ] {
                if let Some(v) = value {
                    *slot = v;
                }
            }
        })
    })
    .await;
    if !matches!(saved, Some(Ok(Some(_)))) {
        return fail(&ctx, Fail::Internal);
    }
    let next = form
        .get("next")
        .filter(|n| !n.is_empty())
        .map(super::auth::safe_next)
        .unwrap_or_else(|| "/.cc/account?saved=1".into());
    Redirect::to(&next).into_response()
}

// ---------------------------------------------------------------------------------------
// Background editor

/// Which background a form changes.
#[derive(Debug, Clone, PartialEq)]
enum Target {
    /// The account's own, for one skin.
    Own(String),
    /// The admin's default for one skin.
    Default(String),
    /// The admin's background for share and public pages.
    Share,
}

impl Target {
    fn parse(s: &str, skins: &[String]) -> Option<Target> {
        if s == "share" {
            return Some(Target::Share);
        }
        let (kind, skin) = s.split_once(':')?;
        if !skins.iter().any(|k| k == skin) {
            return None;
        }
        match kind {
            "user" => Some(Target::Own(skin.to_string())),
            "default" => Some(Target::Default(skin.to_string())),
            _ => None,
        }
    }

    fn code(&self) -> String {
        match self {
            Target::Own(s) => format!("user:{s}"),
            Target::Default(s) => format!("default:{s}"),
            Target::Share => "share".into(),
        }
    }

    fn admin_only(&self) -> bool {
        !matches!(self, Target::Own(_))
    }

    fn href(&self) -> String {
        match self {
            Target::Own(s) => format!("/.cc/account/backgrounds?skin={}", enc(s)),
            Target::Default(s) => format!("/.cc/admin/appearance?edit=default:{}", enc(s)),
            Target::Share => "/.cc/admin/appearance?edit=share".into(),
        }
    }

    fn saved_href(&self) -> String {
        let h = self.href();
        let sep = if h.contains('?') { '&' } else { '?' };
        format!("{h}{sep}saved=1")
    }

    /// The stored background (None: the default, or none for the admin's targets).
    fn stored(&self, a: &Appearance, own: &BTreeMap<String, Background>) -> Option<Background> {
        match self {
            Target::Own(s) => own.get(s).cloned(),
            Target::Default(s) => a.backgrounds.get(s).cloned(),
            Target::Share => a.share_background.clone(),
        }
    }
}

/// May `cu` use set `id` in a background of `target`? Own backgrounds take own sets; the
/// admin's take the admin's own sets and those already shown to everyone.
fn may_use(st: &AppState, cu: &CurrentUser, target: &Target, id: &str) -> bool {
    st.looks
        .set(id)
        .is_some_and(|s| s.owner == cu.key || (target.admin_only() && st.looks.is_public(id)))
}

fn upload_state(st: &AppState, cu: &CurrentUser, count: usize) -> UploadState {
    if cu.role == ADMIN_ROLE {
        UploadState::Allowed
    } else if st.cfg.appearance.deny_upload_roles.contains(&cu.role) {
        UploadState::Denied
    } else if count >= st.cfg.appearance.max_backgrounds_per_user as usize {
        UploadState::LimitReached
    } else {
        UploadState::Allowed
    }
}

const DEFAULT_COLORS: [&str; 3] = ["#1e293b", "#475569", "#94a3b8"];

fn editor_view(
    st: &AppState,
    ctx: &Ctx,
    cu: &CurrentUser,
    target: &Target,
    error: Option<String>,
    saved: bool,
) -> BackgroundsView {
    let a = st.looks.appearance();
    let stored = target.stored(&a, &cu.settings.backgrounds);
    let fill = match stored.as_ref().map(|b| &b.fill) {
        None if matches!(target, Target::Own(_)) => "default",
        None | Some(Fill::None) => "none",
        Some(Fill::Color { .. }) => "color",
        Some(Fill::Images { .. }) => "images",
    };
    let mut colors = DEFAULT_COLORS.map(String::from);
    let (mut stops, mut angle) = (2, 135);
    if let Some(Fill::Color {
        colors: c,
        angle: g,
    }) = stored.as_ref().map(|b| &b.fill)
    {
        for (slot, value) in colors.iter_mut().zip(c) {
            *slot = value.clone();
        }
        (stops, angle) = (c.len(), *g);
    }
    let selected: Vec<String> = stored
        .as_ref()
        .map(|b| look::set_ids(b).to_vec())
        .unwrap_or_default();
    let mut shown = st.looks.sets_of(&cu.key);
    let own_count = shown.len();
    if target.admin_only() {
        // Sets another admin put here stay visible, so saving doesn't drop them.
        for id in &selected {
            if !shown.iter().any(|s| &s.id == id)
                && let Some(s) = st.looks.set(id)
            {
                shown.push(s);
            }
        }
    }
    let sets = shown
        .iter()
        .map(|s| SetCard {
            id: s.id.clone(),
            selected: selected.contains(&s.id),
            deletable: s.owner == cu.key,
            pictures: s
                .images
                .iter()
                .map(|i| {
                    (
                        ctx.tf(&format!("backgrounds.slot_{}", i.slot), &[]),
                        format!("/.cc/bg/{}/{}.small.jpg", s.id, i.slot),
                    )
                })
                .collect(),
        })
        .collect();
    let (dim, blur) = stored
        .as_ref()
        .map(|b| (b.dim, b.blur))
        .unwrap_or((DEFAULT_DIM, 0));
    let skins = skin_names(st);
    let tabs = match target {
        Target::Own(current) => skins
            .iter()
            .map(|k| Tab {
                label: skin_label(ctx, k),
                href: Target::Own(k.clone()).href(),
                current: k == current,
            })
            .collect(),
        _ => skins
            .iter()
            .map(|k| Target::Default(k.clone()))
            .chain([Target::Share])
            .map(|t| Tab {
                label: match &t {
                    Target::Default(k) => skin_label(ctx, k),
                    _ => ctx.t("appearance.share_background").to_string(),
                },
                href: t.href(),
                current: &t == target,
            })
            .collect(),
    };
    let max = st.cfg.appearance.max_backgrounds_per_user;
    let admin = cu.role == ADMIN_ROLE;
    let count = if admin {
        ctx.tn("backgrounds.count_unlimited", own_count as u64)
    } else {
        ctx.tnf(
            "backgrounds.count",
            own_count as u64,
            &[("max", &ctx.number(u64::from(max)))],
        )
    };
    let title = match target {
        Target::Own(k) if skins.len() > 1 => {
            ctx.tf("backgrounds.title_skin", &[("skin", &skin_label(ctx, k))])
        }
        Target::Own(_) => ctx.t("backgrounds.title").to_string(),
        Target::Default(k) => ctx.tf(
            "backgrounds.title_default",
            &[("skin", &skin_label(ctx, k))],
        ),
        Target::Share => ctx.t("backgrounds.title_share").to_string(),
    };
    let back = if target.admin_only() {
        (ctx.t("appearance.title"), "/.cc/admin/appearance")
    } else {
        (ctx.t("account.title"), "/.cc/account")
    };
    BackgroundsView {
        csrf: cu.csrf.clone(),
        title,
        target: target.code(),
        tabs,
        fill,
        allow_default: !target.admin_only(),
        colors,
        stops,
        angle,
        dim,
        blur,
        sets,
        count,
        upload: upload_state(st, cu, own_count),
        limit_text: Some(ctx.tf(
            "backgrounds.limit_reached",
            &[("max", &ctx.number(u64::from(max)))],
        )),
        error,
        saved,
        back: Some((back.0.to_string(), back.1.to_string())),
    }
}

pub async fn backgrounds_page(
    State(st): State<Shared>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let Some(cu) = current.clone() else {
        return Redirect::to("/.cc/login?next=%2F.cc%2Faccount%2Fbackgrounds").into_response();
    };
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    let q = Query::parse(uri.query());
    let skin = q
        .get("skin")
        .filter(|k| st.skins.iter().any(|x| x == k))
        .map(str::to_string)
        .unwrap_or_else(|| ctx.skin.clone());
    let target = Target::Own(skin);
    av::backgrounds(
        &ctx,
        &editor_view(&st, &ctx, &cu, &target, None, q.has("saved")),
    )
    .into_response()
}

fn step(v: Option<&str>) -> Result<u8, ()> {
    match v {
        None | Some("") => Ok(0),
        Some(s) => s.parse::<u8>().ok().filter(|n| *n <= MAX_STEP).ok_or(()),
    }
}

/// The background a save form asks for. Ok(None): the default (own) or none (admin).
/// Err(Some(msg)): show the editor again with that message; Err(None): a bad request.
fn parsed_choice(
    st: &AppState,
    ctx: &Ctx,
    cu: &CurrentUser,
    target: &Target,
    form: &Query,
) -> Result<Option<Background>, Option<String>> {
    let dim = step(form.get("dim")).map_err(|_| None)?;
    let blur = step(form.get("blur")).map_err(|_| None)?;
    let fill = match form.get("fill").unwrap_or("") {
        "default" if !target.admin_only() => return Ok(None),
        "none" if target.admin_only() => return Ok(None),
        "none" => Fill::None,
        "color" => {
            let stops: usize = form
                .get("stops")
                .and_then(|s| s.parse().ok())
                .filter(|n| (1..=look::MAX_COLORS).contains(n))
                .ok_or(None)?;
            let colors: Vec<&str> = (1..=stops)
                .map(|i| form.get(&format!("c{i}")).unwrap_or(""))
                .collect();
            let angle: u16 = match form.get("angle") {
                None | Some("") => 0,
                Some(a) => a.parse().ok().filter(|a| *a < 360).ok_or(None)?,
            };
            look::color_fill(&colors, angle).ok_or(None)?
        }
        "images" => {
            let mut sets: Vec<String> = Vec::new();
            for id in form.all("sets") {
                if may_use(st, cu, target, id) && !sets.iter().any(|s| s == id) {
                    sets.push(id.to_string());
                }
            }
            if sets.is_empty() {
                return Err(Some(ctx.t("backgrounds.error_pick").to_string()));
            }
            Fill::Images { sets }
        }
        _ => return Err(None),
    };
    Ok(Some(Background { fill, dim, blur }))
}

/// Store `choice` for `target` (blocking). Own backgrounds go to the account, the others
/// to the appearance settings.
fn store_choice(
    st: &AppState,
    user: &str,
    target: &Target,
    choice: impl FnOnce(Option<Background>) -> Option<Background>,
) -> Result<(), ()> {
    match target {
        Target::Own(skin) => {
            let updated = st.db.update_user(user, |u| {
                let old = u.settings.backgrounds.remove(skin);
                if let Some(bg) = choice(old) {
                    u.settings.backgrounds.insert(skin.clone(), look::clamp(bg));
                }
            });
            matches!(updated, Ok(Some(_))).then_some(()).ok_or(())
        }
        Target::Default(skin) => {
            let mut a = (*st.looks.appearance()).clone();
            let old = a.backgrounds.remove(skin);
            if let Some(bg) = choice(old) {
                a.backgrounds.insert(skin.clone(), look::clamp(bg));
            }
            st.looks.set_appearance(&st.db, a).map_err(|_| ())
        }
        Target::Share => {
            let mut a = (*st.looks.appearance()).clone();
            a.share_background = choice(a.share_background.take()).map(look::clamp);
            st.looks.set_appearance(&st.db, a).map_err(|_| ())
        }
    }
}

/// The checked target of a form, or the response that refuses it.
fn form_target(
    st: &AppState,
    ctx: &Ctx,
    principal: &Principal,
    code: Option<&str>,
) -> Result<Target, Box<Response>> {
    let target = code
        .and_then(|t| Target::parse(t, &skin_names(st)))
        .ok_or_else(|| Box::new(fail(ctx, Fail::BadRequest)))?;
    if target.admin_only() && !principal.is_admin() {
        return Err(Box::new(forbidden(ctx)));
    }
    Ok(target)
}

/// POST `/.cc/account/backgrounds`: save a background, or delete a picture set.
pub async fn backgrounds_submit(
    State(st): State<Shared>,
    Extension(client): Extension<ClientInfo>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let Some(cu) = current.clone() else {
        return Redirect::to(LOGIN_FOR_ACCOUNT).into_response();
    };
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    let form = Query::parse(Some(&body));
    if !csrf_ok(&headers, &st, &cu, form.get("csrf").unwrap_or("")) {
        return forbidden(&ctx);
    }
    let target = match form_target(&st, &ctx, &principal, form.get("target")) {
        Ok(t) => t,
        Err(res) => return *res,
    };
    let actor = format!("user:{}", cu.key);

    if let Some(id) = form.get("delete").filter(|s| !s.is_empty()) {
        if st.looks.set(id).is_none_or(|s| s.owner != cu.key) {
            return fail(&ctx, Fail::NotFound);
        }
        let (st2, id2) = (st.clone(), id.to_string());
        if !matches!(
            blocking(move || st2.looks.remove(&st2.db, &id2)).await,
            Some(Ok(()))
        ) {
            return fail(&ctx, Fail::Internal);
        }
        audit(&st, actor, client.ip, "background_deleted", id, "");
        return Redirect::to(&target.href()).into_response();
    }

    let choice = match parsed_choice(&st, &ctx, &cu, &target, &form) {
        Ok(c) => c,
        Err(None) => return fail(&ctx, Fail::BadRequest),
        Err(Some(msg)) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                av::backgrounds(
                    &ctx,
                    &editor_view(&st, &ctx, &cu, &target, Some(msg), false),
                ),
            )
                .into_response();
        }
    };
    let (st2, key, t2) = (st.clone(), cu.key.clone(), target.clone());
    if !matches!(
        blocking(move || store_choice(&st2, &key, &t2, |_| choice)).await,
        Some(Ok(()))
    ) {
        return fail(&ctx, Fail::Internal);
    }
    if target.admin_only() {
        audit(
            &st,
            actor,
            client.ip,
            "appearance_changed",
            &target.code(),
            "",
        );
    }
    Redirect::to(&target.saved_href()).into_response()
}

/// POST `/.cc/account/backgrounds/upload` (multipart): add a picture set and show it in
/// the background the form came from.
pub async fn upload(
    State(st): State<Shared>,
    Extension(client): Extension<ClientInfo>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    let Some(cu) = current.clone() else {
        return Redirect::to(LOGIN_FOR_ACCOUNT).into_response();
    };
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    if !same_origin(&headers, &st.cfg.server.public_url) {
        return forbidden(&ctx);
    }
    let (mut csrf, mut target_code) = (String::new(), String::new());
    let mut pictures: Vec<(String, Vec<u8>)> = Vec::new();
    let mut too_large: Option<String> = None;
    let mut broken = false;
    'fields: loop {
        let mut field = match multipart.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(_) => {
                broken = true;
                break;
            }
        };
        let name = field.name().unwrap_or_default().to_string();
        match name.as_str() {
            "csrf" => csrf = field.text().await.unwrap_or_default(),
            "target" => target_code = field.text().await.unwrap_or_default(),
            slot if look::is_slot(slot) => {
                let mut buf = Vec::new();
                loop {
                    match field.chunk().await {
                        Ok(Some(c)) if buf.len() + c.len() > look::MAX_UPLOAD_BYTES => {
                            too_large = Some(slot.to_string());
                            break 'fields;
                        }
                        Ok(Some(c)) => buf.extend_from_slice(&c),
                        Ok(None) => break,
                        Err(_) => {
                            broken = true;
                            break 'fields;
                        }
                    }
                }
                pictures.push((slot.to_string(), buf));
            }
            _ => {}
        }
    }
    if !csrf_ok(&headers, &st, &cu, &csrf) {
        return forbidden(&ctx);
    }
    let target = match form_target(&st, &ctx, &principal, Some(&target_code)) {
        Ok(t) => t,
        Err(res) => return *res,
    };
    let slot_label = |slot: &str| ctx.tf(&format!("backgrounds.slot_{slot}"), &[]);
    let refuse = |msg: String| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            av::backgrounds(
                &ctx,
                &editor_view(&st, &ctx, &cu, &target, Some(msg), false),
            ),
        )
            .into_response()
    };
    if let Some(slot) = too_large {
        return refuse(ctx.tf(
            "backgrounds.error_too_large",
            &[("slot", &slot_label(&slot))],
        ));
    }
    if broken {
        return refuse(ctx.t("backgrounds.error_incomplete").to_string());
    }
    match upload_state(&st, &cu, st.looks.sets_of(&cu.key).len()) {
        UploadState::Allowed => {}
        UploadState::Denied => return refuse(ctx.t("backgrounds.upload_denied").to_string()),
        UploadState::LimitReached => {
            let max = ctx.number(u64::from(st.cfg.appearance.max_backgrounds_per_user));
            return refuse(ctx.tf("backgrounds.limit_reached", &[("max", &max)]));
        }
    }
    let set = match st
        .looks
        .add(
            st.db.clone(),
            &cu.key,
            pictures,
            jiff::Timestamp::now().as_millisecond(),
        )
        .await
    {
        Ok(set) => set,
        Err(e) => {
            let msg = match &e {
                AddError::Empty => ctx.t("backgrounds.error_empty").to_string(),
                AddError::TooLarge(slot) => ctx.tf(
                    "backgrounds.error_too_large",
                    &[("slot", &slot_label(slot))],
                ),
                AddError::NotAPicture(slot) => ctx.tf(
                    "backgrounds.error_not_picture",
                    &[("slot", &slot_label(slot))],
                ),
                AddError::Busy | AddError::Storage(_) => {
                    tracing::error!(user = cu.key, "background upload failed: {e}");
                    ctx.t("backgrounds.error_failed").to_string()
                }
            };
            return refuse(msg);
        }
    };
    let slots: Vec<&str> = set.images.iter().map(|i| i.slot.as_str()).collect();
    audit(
        &st,
        format!("user:{}", cu.key),
        client.ip,
        "background_added",
        &set.id,
        &slots.join(","),
    );
    // Show the new set right away: added to the pictures in use, or as the only one.
    let (st2, key, t2, id) = (st.clone(), cu.key.clone(), target.clone(), set.id.clone());
    let shown = blocking(move || {
        store_choice(&st2, &key, &t2, |old| {
            Some(match old {
                Some(Background {
                    fill: Fill::Images { mut sets },
                    dim,
                    blur,
                }) => {
                    sets.push(id);
                    Background {
                        fill: Fill::Images { sets },
                        dim,
                        blur,
                    }
                }
                old => Background {
                    fill: Fill::Images { sets: vec![id] },
                    dim: old.map_or(DEFAULT_DIM, |b| b.dim.max(DEFAULT_DIM)),
                    blur: 0,
                },
            })
        })
    })
    .await;
    if !matches!(shown, Some(Ok(()))) {
        tracing::warn!(user = cu.key, "new background set not selected");
    }
    if target.admin_only() {
        audit(
            &st,
            format!("user:{}", cu.key),
            client.ip,
            "appearance_changed",
            &target.code(),
            "",
        );
    }
    Redirect::to(&target.saved_href()).into_response()
}

// ---------------------------------------------------------------------------------------
// Admin: appearance

fn describe(ctx: &Ctx, bg: Option<&Background>) -> String {
    match bg.map(|b| &b.fill) {
        None | Some(Fill::None) => ctx.t("backgrounds.fill_none").to_string(),
        Some(Fill::Color { .. }) => ctx.t("backgrounds.fill_color").to_string(),
        Some(Fill::Images { sets }) => ctx.tn("appearance.sets", sets.len() as u64),
    }
}

fn appearance_view(st: &AppState, ctx: &Ctx, cu: &CurrentUser, saved: bool) -> AppearanceView {
    let a = st.looks.appearance();
    let default_skin = look::pick_skin(&st.skins, a.default_skin.as_deref(), None);
    let share_skin = look::pick_skin(&st.skins, a.share_skin.as_deref(), Some(&default_skin));
    let skin_opts = |current: &str| -> Vec<Opt> {
        if st.skins.len() > 1 {
            st.skins
                .iter()
                .map(|k| opt(k, skin_label(ctx, k), k == current))
                .collect()
        } else {
            Vec::new()
        }
    };
    let mode = a.rotation.as_deref().unwrap_or(DEFAULT_ROTATION);
    let rotations = look::ROTATIONS
        .iter()
        .map(|m| opt(m, rotation_label(ctx, m), *m == mode))
        .collect();
    let mut backgrounds: Vec<BackgroundRow> = skin_names(st)
        .into_iter()
        .map(|k| BackgroundRow {
            label: skin_label(ctx, &k),
            desc: describe(ctx, a.backgrounds.get(&k)),
            href: Target::Default(k).href(),
        })
        .collect();
    backgrounds.push(BackgroundRow {
        label: ctx.t("appearance.share_background").to_string(),
        desc: describe(ctx, a.share_background.as_ref()),
        href: Target::Share.href(),
    });
    let denied = &st.cfg.appearance.deny_upload_roles;
    AppearanceView {
        csrf: cu.csrf.clone(),
        saved,
        default_skins: skin_opts(&default_skin),
        share_skins: skin_opts(&share_skin),
        rotations,
        backgrounds,
        limit: ctx.tf(
            "appearance.limit",
            &[(
                "max",
                &ctx.number(u64::from(st.cfg.appearance.max_backgrounds_per_user)),
            )],
        ),
        denied: if denied.is_empty() {
            ctx.t("appearance.denied_none").to_string()
        } else {
            let roles: Vec<&str> = denied.iter().map(String::as_str).collect();
            ctx.tf("appearance.denied", &[("roles", &roles.join(", "))])
        },
    }
}

/// `/.cc/admin/appearance`, or with `?edit=default:<skin>` / `?edit=share` the editor of
/// that background.
pub async fn appearance_page(
    State(st): State<Shared>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    let Some(cu) = current.as_deref().filter(|_| principal.is_admin()) else {
        return fail(&ctx, Fail::NotFound);
    };
    let q = Query::parse(uri.query());
    if let Some(edit) = q.get("edit") {
        return match Target::parse(edit, &skin_names(&st)).filter(Target::admin_only) {
            Some(t) => av::backgrounds(&ctx, &editor_view(&st, &ctx, cu, &t, None, q.has("saved")))
                .into_response(),
            None => fail(&ctx, Fail::NotFound),
        };
    }
    av::appearance(&ctx, &appearance_view(&st, &ctx, cu, q.has("saved"))).into_response()
}

/// POST `/.cc/admin/appearance`: default skin, share skin, default rotation.
pub async fn appearance_submit(
    State(st): State<Shared>,
    Extension(client): Extension<ClientInfo>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    let Some(cu) = current.as_deref().filter(|_| principal.is_admin()) else {
        return fail(&ctx, Fail::NotFound);
    };
    let form = Query::parse(Some(&body));
    if !csrf_ok(&headers, &st, cu, form.get("csrf").unwrap_or("")) {
        return forbidden(&ctx);
    }
    let skin = |key: &str| change(&form, key, &[], |k| st.skins.iter().any(|x| x == k));
    let (Ok(default_skin), Ok(share_skin), Ok(rotation)) = (
        skin("default_skin"),
        skin("share_skin"),
        change(&form, "rotation", &[], look::is_rotation),
    ) else {
        return fail(&ctx, Fail::BadRequest);
    };
    let st2 = st.clone();
    let saved = blocking(move || {
        let mut a = (*st2.looks.appearance()).clone();
        for (slot, value) in [
            (&mut a.default_skin, default_skin),
            (&mut a.share_skin, share_skin),
            (&mut a.rotation, rotation),
        ] {
            if let Some(v) = value {
                *slot = v;
            }
        }
        st2.looks.set_appearance(&st2.db, a)
    })
    .await;
    if !matches!(saved, Some(Ok(()))) {
        return fail(&ctx, Fail::Internal);
    }
    audit(
        &st,
        format!("user:{}", cu.key),
        client.ip,
        "appearance_changed",
        "settings",
        "",
    );
    Redirect::to("/.cc/admin/appearance?saved=1").into_response()
}
