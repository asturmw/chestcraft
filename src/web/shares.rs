//! The owner's side of share links: create (`/.cc/shares/new?vol=…&path=…`), list
//! (`/.cc/shares`; the admin sees everyone's with `?all=1`), and per share
//! (`/.cc/shares/<id>`) the details with the access log, edit (mode, expiry, note, size
//! limit), a new password (shown once) and revoke. Only the creator and the admin manage a
//! share; a new mode is checked against the creator's rights, whoever edits.

use super::auth::{audit, csrf_ok, forbidden, now};
use super::files::enc;
use super::share_views::{
    self, DetailView, ExpiryChoice, LogRow, Message, ModeChoice, NewShareView, SecretView,
    ShareRow, SharesView,
};
use super::views::{self, Ctx};
use super::{AppState, ClientInfo, Current, CurrentUser, Shared, page_ctx};
use crate::auth::perm::Principal;
use crate::db::{Share, User};
use crate::fs::path::RelPath;
use crate::i18n::{DE, EN, Lang};
use crate::shares::{self, Expiry, Mode, NewShare, Password, Pause, ShareError, Status};
use axum::extract::{Extension, Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;

const SIGN_IN: &str = "/.cc/login?next=%2F.cc%2Fshares";

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    tokio::task::spawn_blocking(f).await.ok()
}

pub fn mode_label<'a>(ctx: &Ctx<'a>, mode: &str) -> &'a str {
    match mode {
        "view_upload" => ctx.t("share_mode.view_upload"),
        "upload" => ctx.t("share_mode.upload"),
        _ => ctx.t("share_mode.view"),
    }
}

fn expiry_label<'a>(ctx: &Ctx<'a>, e: Expiry) -> &'a str {
    match e {
        Expiry::Day => ctx.t("share_expiry.day"),
        Expiry::Week => ctx.t("share_expiry.week"),
        Expiry::Month => ctx.t("share_expiry.month"),
        Expiry::Never => ctx.t("share_expiry.never"),
    }
}

pub fn expiry_choices(ctx: &Ctx, selected: Expiry) -> Vec<ExpiryChoice> {
    Expiry::ALL
        .iter()
        .map(|e| ExpiryChoice {
            value: e.as_str(),
            label: expiry_label(ctx, *e).to_string(),
            checked: *e == selected,
        })
        .collect()
}

fn status_text(ctx: &Ctx, s: Status) -> String {
    ctx.t(match s {
        Status::Active => "share_status.active",
        Status::Expired => "share_status.expired",
        Status::Revoked => "share_status.revoked",
        Status::Paused(Pause::Owner) => "share_status.paused_owner",
        Status::Paused(Pause::Rights) => "share_status.paused_rights",
        Status::Paused(Pause::Volume) => "share_status.paused_volume",
        Status::Paused(Pause::Trash) => "share_status.paused_trash",
        Status::Missing => "share_status.missing",
    })
    .to_string()
}

fn counts(ctx: &Ctx, s: &Share) -> String {
    ctx.tf(
        "shares.counts",
        &[
            ("opens", &s.opens.to_string()),
            ("downloads", &s.downloads.to_string()),
            ("uploads", &s.uploads.to_string()),
        ],
    )
}

fn target_of(s: &Share) -> (String, String) {
    let path = RelPath::parse(&s.path).unwrap_or_default();
    let label = if path.is_root() {
        s.volume.clone()
    } else {
        format!("{}/{}", s.volume, path.as_str())
    };
    let href = format!(
        "/{}/{}{}",
        enc(&s.volume),
        path.to_url(),
        if s.is_dir && !path.is_root() { "/" } else { "" }
    );
    (label, href)
}

fn link_of(st: &AppState, s: &Share) -> String {
    format!("{}/s/{}", st.cfg.server.public_url, s.id)
}

fn may_manage(cu: &CurrentUser, principal: &Principal, s: &Share) -> bool {
    principal.is_admin() || s.owner == cu.key
}

fn problem(ctx: &Ctx, status: StatusCode, msg: &str, back: &str) -> Response {
    (
        status,
        views::problems(ctx, ctx.t("ops.failed_title"), msg, &[], Some(back)),
    )
        .into_response()
}

fn share_error(ctx: &Ctx, e: &ShareError) -> String {
    match e {
        ShareError::NoVolume | ShareError::NotFound => ctx.t("share_create.not_found").into(),
        ShareError::NotRemote => ctx.t("browser.share_local_only").into(),
        ShareError::NoRights => ctx.t("share_create.no_rights").into(),
        ShareError::UploadNeedsFolder => ctx.t("share_create.upload_needs_folder").into(),
        ShareError::PasswordTooShort => ctx.tf(
            "share_create.password_too_short",
            &[("min", &shares::MIN_OWN_PASSWORD.to_string())],
        ),
        ShareError::NoteTooLong => ctx.t("share_create.note_too_long").into(),
        ShareError::TooMany => ctx.t("share_create.too_many").into(),
        ShareError::Db(e) => {
            tracing::error!("share: {e}");
            ctx.t("ops.failed").into()
        }
    }
}

// ---------------------------------------------------------------------------------------
// List

#[derive(Deserialize, Default)]
pub struct ListQuery {
    #[serde(default)]
    all: String,
}

pub async fn list(
    State(st): State<Shared>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
    Query(q): Query<ListQuery>,
) -> Response {
    let Some(cu) = current.clone() else {
        return Redirect::to(SIGN_IN).into_response();
    };
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    let all = principal.is_admin() && q.all == "1";
    let loaded = {
        let (st, key) = (st.clone(), cu.key.clone());
        blocking(
            move || -> crate::db::Result<Vec<(Share, Status, Option<User>)>> {
                let mut out = Vec::new();
                for s in st.db.shares()? {
                    if !all && s.owner != key {
                        continue;
                    }
                    let (status, owner) = shares::status(&st.cfg, &st.volumes, &st.db, &s, now())?;
                    out.push((s, status, owner));
                }
                out.sort_by(|a, b| b.0.created.cmp(&a.0.created).then(a.0.id.cmp(&b.0.id)));
                Ok(out)
            },
        )
        .await
    };
    let Some(Ok(loaded)) = loaded else {
        return problem(
            &ctx,
            StatusCode::INTERNAL_SERVER_ERROR,
            ctx.t("ops.failed"),
            "/",
        );
    };
    let rows = loaded
        .iter()
        .map(|(s, status, owner)| {
            let (target, target_href) = target_of(s);
            ShareRow {
                target,
                target_href,
                mode: mode_label(&ctx, &s.mode).to_string(),
                created: ctx.datetime(s.created),
                expires: s
                    .expires
                    .map(|t| ctx.datetime(t))
                    .unwrap_or_else(|| ctx.t("share_expiry.never").to_string()),
                status_code: status.code(),
                status: status_text(&ctx, *status),
                last_opened: s
                    .last_opened
                    .map(|t| ctx.datetime(t))
                    .unwrap_or_else(|| ctx.t("shares.never_opened").to_string()),
                counts: counts(&ctx, s),
                owner: all.then(|| {
                    owner
                        .as_ref()
                        .map(|u| u.name.clone())
                        .unwrap_or_else(|| s.owner.clone())
                }),
                detail_href: format!("/.cc/shares/{}", s.id),
            }
        })
        .collect();
    let toggle = principal.is_admin().then(|| {
        if all {
            (
                ctx.t("shares.show_mine").to_string(),
                "/.cc/shares".to_string(),
            )
        } else {
            (
                ctx.t("shares.show_all").to_string(),
                "/.cc/shares?all=1".to_string(),
            )
        }
    });
    share_views::shares_list(&ctx, &SharesView { all, toggle, rows }).into_response()
}

// ---------------------------------------------------------------------------------------
// Create

#[derive(Deserialize, Default)]
pub struct NewQuery {
    #[serde(default)]
    vol: String,
    #[serde(default)]
    path: String,
}

/// Modes `principal` may use for this target (none: the reason).
fn allowed_modes(
    st: &AppState,
    principal: &Principal,
    vol: &str,
    path: &RelPath,
    is_dir: bool,
) -> Result<Vec<Mode>, ShareError> {
    let mut ok = Vec::new();
    let mut first_err = None;
    for m in Mode::ALL {
        if m.uploads() && !is_dir {
            continue;
        }
        match shares::check_rights(&st.cfg, principal, vol, path, m) {
            Ok(()) => ok.push(m),
            Err(e) => {
                first_err.get_or_insert(e);
            }
        }
    }
    match (ok.is_empty(), first_err) {
        (true, Some(e)) => Err(e),
        _ => Ok(ok),
    }
}

struct Target {
    vol: String,
    path: RelPath,
    is_dir: bool,
}

async fn target(st: &Shared, vol: &str, path: &str) -> Result<Target, ShareError> {
    let path = RelPath::parse(path).map_err(|_| ShareError::NotFound)?;
    let v = st.volumes.get(vol).cloned().ok_or(ShareError::NoVolume)?;
    let p2 = path.clone();
    let meta = blocking(move || v.stat(&p2))
        .await
        .and_then(|r| r.ok())
        .ok_or(ShareError::NotFound)?;
    let is_dir = match meta.kind {
        crate::fs::volume::Kind::Dir => true,
        crate::fs::volume::Kind::File => false,
        crate::fs::volume::Kind::Other => return Err(ShareError::NotFound),
    };
    Ok(Target {
        vol: vol.to_string(),
        path,
        is_dir,
    })
}

#[allow(clippy::too_many_arguments)]
fn new_view(
    ctx: &Ctx,
    cu: &CurrentUser,
    t: &Target,
    modes: &[Mode],
    mode: Mode,
    expiry: Expiry,
    passphrase: String,
    own: bool,
    note: String,
    max_mib: String,
    error: Option<String>,
) -> NewShareView {
    let parent = t.path.parent().unwrap_or_default();
    let location = if parent.is_root() {
        t.vol.clone()
    } else {
        format!("{}/{}", t.vol, parent.as_str())
    };
    NewShareView {
        csrf: cu.csrf.clone(),
        vol: t.vol.clone(),
        path: t.path.as_str().to_string(),
        name: t.path.name().unwrap_or(&t.vol).to_string(),
        location_href: if parent.is_root() {
            format!("/{}/", enc(&t.vol))
        } else {
            format!("/{}/{}/", enc(&t.vol), parent.to_url())
        },
        location,
        modes: modes
            .iter()
            .map(|m| ModeChoice {
                value: m.as_str(),
                label: mode_label(ctx, m.as_str()).to_string(),
                checked: *m == mode,
            })
            .collect(),
        expiries: expiry_choices(ctx, expiry),
        passphrase,
        own,
        note,
        max_mib,
        error,
    }
}

pub async fn new_page(
    State(st): State<Shared>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
    Query(q): Query<NewQuery>,
) -> Response {
    let Some(cu) = current.clone() else {
        return Redirect::to(SIGN_IN).into_response();
    };
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    let t = match target(&st, &q.vol, &q.path).await {
        Ok(t) => t,
        Err(e) => return problem(&ctx, StatusCode::NOT_FOUND, &share_error(&ctx, &e), "/"),
    };
    let modes = match allowed_modes(&st, &principal, &t.vol, &t.path, t.is_dir) {
        Ok(m) => m,
        Err(e) => return problem(&ctx, StatusCode::FORBIDDEN, &share_error(&ctx, &e), "/"),
    };
    let passphrase = st.words.passphrase(ctx.code());
    let view = new_view(
        &ctx,
        &cu,
        &t,
        &modes,
        Mode::View,
        Expiry::Month,
        passphrase,
        false,
        String::new(),
        String::new(),
        None,
    );
    share_views::share_new(&ctx, &view).into_response()
}

#[derive(Deserialize, Default)]
pub struct NewForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    op: String,
    vol: String,
    path: String,
    #[serde(default)]
    mode: String,
    #[serde(default)]
    expiry: String,
    #[serde(default)]
    password_kind: String,
    #[serde(default)]
    passphrase: String,
    #[serde(default)]
    own_password: String,
    #[serde(default)]
    note: String,
    #[serde(default)]
    max_mib: String,
}

/// "" = no limit; otherwise whole MiB.
fn parse_mib(s: &str) -> Result<Option<u64>, ()> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    match s.parse::<u64>() {
        Ok(n) if (1..=(1 << 24)).contains(&n) => Ok(Some(n << 20)),
        _ => Err(()),
    }
}

/// Link + password page, with a ready-to-send message in the creator's language and the
/// other built-in one.
fn secret_page(
    st: &AppState,
    ctx: &Ctx,
    title: String,
    link: String,
    password: String,
    until: Option<i64>,
    back: (String, String),
) -> Response {
    let first = if ctx.lang == DE { DE } else { EN };
    let langs: [Lang; 2] = if first == DE { [DE, EN] } else { [EN, DE] };
    let tz = &st.cfg.server.timezone;
    let messages = langs
        .iter()
        .map(|&lang| {
            let text = match until {
                Some(t) => st.i18n.tf(
                    lang,
                    "share_created.message_text",
                    &[
                        ("link", &link),
                        ("password", &password),
                        ("date", &st.i18n.datetime(lang, t, tz)),
                    ],
                ),
                None => st.i18n.tf(
                    lang,
                    "share_created.message_text_never",
                    &[("link", &link), ("password", &password)],
                ),
            };
            Message {
                lang_name: st.i18n.t(lang, "meta.name").to_string(),
                code: st.i18n.code(lang).to_string(),
                text,
            }
        })
        .collect();
    let mut ctx2 = Ctx {
        scripts: ctx.scripts.clone(),
        lang_switch: None,
        ..*ctx
    };
    ctx2.scripts.push("copy.js");
    let view = SecretView {
        title,
        until: until
            .map(|t| ctx.datetime(t))
            .unwrap_or_else(|| ctx.t("share_expiry.never").to_string()),
        link,
        password,
        messages,
        back_href: back.0,
        back_label: back.1,
    };
    share_views::secret(&ctx2, &view).into_response()
}

pub async fn new_submit(
    State(st): State<Shared>,
    Extension(client): Extension<ClientInfo>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
    Form(form): Form<NewForm>,
) -> Response {
    let Some(cu) = current.clone() else {
        return Redirect::to(SIGN_IN).into_response();
    };
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    if !csrf_ok(&headers, &st, &cu, &form.csrf) {
        return forbidden(&ctx);
    }
    let t = match target(&st, &form.vol, &form.path).await {
        Ok(t) => t,
        Err(e) => return problem(&ctx, StatusCode::NOT_FOUND, &share_error(&ctx, &e), "/"),
    };
    let modes = match allowed_modes(&st, &principal, &t.vol, &t.path, t.is_dir) {
        Ok(m) => m,
        Err(e) => return problem(&ctx, StatusCode::FORBIDDEN, &share_error(&ctx, &e), "/"),
    };
    let mode = Mode::parse(&form.mode)
        .filter(|m| modes.contains(m))
        .unwrap_or(Mode::View);
    let expiry = Expiry::parse(&form.expiry).unwrap_or(Expiry::Month);
    let own = form.password_kind == "own";
    let rerender = |passphrase: String, error: Option<String>, status: StatusCode| {
        let view = new_view(
            &ctx,
            &cu,
            &t,
            &modes,
            mode,
            expiry,
            passphrase,
            own,
            form.note.clone(),
            form.max_mib.clone(),
            error,
        );
        (status, share_views::share_new(&ctx, &view)).into_response()
    };
    let passphrase = if form.passphrase.trim().is_empty() || form.op == "regenerate" {
        st.words.passphrase(ctx.code())
    } else {
        form.passphrase.trim().to_string()
    };
    if form.op == "regenerate" {
        return rerender(passphrase, None, StatusCode::OK);
    }
    let Ok(max_file) = parse_mib(&form.max_mib) else {
        return rerender(
            passphrase,
            Some(ctx.t("share_create.bad_size").to_string()),
            StatusCode::UNPROCESSABLE_ENTITY,
        );
    };
    let password = if own {
        Password::Own(form.own_password.clone())
    } else {
        Password::Passphrase(passphrase.clone())
    };
    let created = {
        let (st, key, t_vol, t_path, note, pw) = (
            st.clone(),
            cu.key.clone(),
            t.vol.clone(),
            t.path.clone(),
            form.note.clone(),
            password.clone(),
        );
        blocking(move || -> Result<Share, ShareError> {
            let owner = st.db.user(&key)?.ok_or(ShareError::NoRights)?;
            shares::create(
                &st.cfg,
                &st.volumes,
                &st.db,
                &NewShare {
                    owner: &owner,
                    volume: &t_vol,
                    path: &t_path,
                    mode,
                    expiry,
                    password: &pw,
                    note: &note,
                    max_file,
                },
                now(),
            )
        })
        .await
    };
    let share = match created {
        Some(Ok(s)) => s,
        Some(Err(e)) => {
            return rerender(
                passphrase,
                Some(share_error(&ctx, &e)),
                StatusCode::UNPROCESSABLE_ENTITY,
            );
        }
        None => {
            return rerender(
                passphrase,
                Some(ctx.t("ops.failed").into()),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };
    audit(
        &st,
        format!("user:{}", cu.key),
        client.ip,
        "share_created",
        &format!("{}/{}", share.volume, share.path),
        &format!("{} {}", share.id, share.mode),
    );
    secret_page(
        &st,
        &ctx,
        ctx.t("share_created.title").to_string(),
        link_of(&st, &share),
        password.text().to_string(),
        share.expires,
        (
            format!("/.cc/shares/{}", share.id),
            ctx.t("share_created.to_details").to_string(),
        ),
    )
}

// ---------------------------------------------------------------------------------------
// Details, edit, new password, revoke

async fn load(
    st: &Shared,
    id: &str,
) -> Option<(Share, Status, Option<User>, Vec<crate::db::ShareLogEntry>)> {
    if !shares::plausible_id(id) {
        return None;
    }
    let (st, id) = (st.clone(), id.to_string());
    blocking(move || {
        let s = st.db.share(&id).ok()??;
        let (status, owner) = shares::status(&st.cfg, &st.volumes, &st.db, &s, now()).ok()?;
        let log = st.db.share_log_of(&id, 200).unwrap_or_default();
        Some((s, status, owner, log))
    })
    .await
    .flatten()
}

fn detail_view(
    st: &AppState,
    ctx: &Ctx,
    cu: &CurrentUser,
    loaded: &(Share, Status, Option<User>, Vec<crate::db::ShareLogEntry>),
    error: Option<String>,
    saved: bool,
) -> DetailView {
    let (s, status, owner, log) = loaded;
    let (target, target_href) = target_of(s);
    let modes: Vec<ModeChoice> = Mode::ALL
        .iter()
        .filter(|m| s.is_dir || !m.uploads())
        .map(|m| ModeChoice {
            value: m.as_str(),
            label: mode_label(ctx, m.as_str()).to_string(),
            checked: m.as_str() == s.mode,
        })
        .collect();
    DetailView {
        id: s.id.clone(),
        csrf: cu.csrf.clone(),
        target,
        target_href,
        link: link_of(st, s),
        owner: owner
            .as_ref()
            .map(|u| u.name.clone())
            .unwrap_or_else(|| s.owner.clone()),
        status_code: status.code(),
        status: status_text(ctx, *status),
        mode: mode_label(ctx, &s.mode).to_string(),
        created: ctx.datetime(s.created),
        expires: s
            .expires
            .map(|t| ctx.datetime(t))
            .unwrap_or_else(|| ctx.t("share_expiry.never").to_string()),
        note: s.note.clone(),
        counts: counts(ctx, s),
        editable: s.revoked.is_none(),
        modes,
        expiries: expiry_choices(ctx, Expiry::Month)
            .into_iter()
            .map(|mut e| {
                e.checked = false;
                e
            })
            .collect(),
        max_mib: s
            .max_file
            .map(|b| (b >> 20).to_string())
            .unwrap_or_default(),
        log: log
            .iter()
            .map(|e| LogRow {
                time: ctx.datetime(e.time),
                ip: e.ip.clone(),
                country: e.country.clone(),
                action: ctx
                    .t(match e.action.as_str() {
                        "unlock" => "share_log.unlock",
                        "unlock_failed" => "share_log.unlock_failed",
                        "download" => "share_log.download",
                        "zip" => "share_log.zip",
                        "upload" => "share_log.upload",
                        _ => "share_log.other",
                    })
                    .to_string(),
                file: e.file.clone(),
            })
            .collect(),
        error,
        saved,
    }
}

pub async fn detail(
    State(st): State<Shared>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let Some(cu) = current.clone() else {
        return Redirect::to(SIGN_IN).into_response();
    };
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    match load(&st, &id).await {
        Some(l) if may_manage(&cu, &principal, &l.0) => {
            share_views::share_detail(&ctx, &detail_view(&st, &ctx, &cu, &l, None, false))
                .into_response()
        }
        _ => problem(
            &ctx,
            StatusCode::NOT_FOUND,
            ctx.t("share_detail.not_found"),
            "/.cc/shares",
        ),
    }
}

#[derive(Deserialize, Default)]
pub struct DetailForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    op: String,
    #[serde(default)]
    mode: String,
    #[serde(default)]
    expiry: String,
    #[serde(default)]
    note: String,
    #[serde(default)]
    max_mib: String,
    #[serde(default)]
    confirm: String,
}

pub async fn detail_submit(
    State(st): State<Shared>,
    Extension(client): Extension<ClientInfo>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(form): Form<DetailForm>,
) -> Response {
    let Some(cu) = current.clone() else {
        return Redirect::to(SIGN_IN).into_response();
    };
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    if !csrf_ok(&headers, &st, &cu, &form.csrf) {
        return forbidden(&ctx);
    }
    let not_found = || {
        problem(
            &ctx,
            StatusCode::NOT_FOUND,
            ctx.t("share_detail.not_found"),
            "/.cc/shares",
        )
    };
    let Some(loaded) = load(&st, &id).await else {
        return not_found();
    };
    if !may_manage(&cu, &principal, &loaded.0) {
        return not_found();
    }
    let share = loaded.0.clone();
    let back = format!("/.cc/shares/{id}");
    if share.revoked.is_some() {
        return problem(
            &ctx,
            StatusCode::CONFLICT,
            ctx.t("share_detail.is_revoked"),
            &back,
        );
    }
    let actor = format!("user:{}", cu.key);
    let target = format!("{}/{}", share.volume, share.path);
    let rerender = |l: &(Share, Status, Option<User>, Vec<crate::db::ShareLogEntry>),
                    error: Option<String>,
                    saved: bool| {
        let status = if error.is_some() {
            StatusCode::UNPROCESSABLE_ENTITY
        } else {
            StatusCode::OK
        };
        (
            status,
            share_views::share_detail(&ctx, &detail_view(&st, &ctx, &cu, l, error, saved)),
        )
            .into_response()
    };
    match form.op.as_str() {
        "edit" => {
            let Some(mode) = Mode::parse(&form.mode) else {
                return rerender(&loaded, Some(ctx.t("ops.invalid").into()), false);
            };
            if mode.uploads() && !share.is_dir {
                return rerender(
                    &loaded,
                    Some(ctx.t("share_create.upload_needs_folder").into()),
                    false,
                );
            }
            // The share never grants more than its creator has.
            let creator_ok = loaded.2.as_ref().is_some_and(|u| {
                shares::check_rights(
                    &st.cfg,
                    &shares::owner_principal(u),
                    &share.volume,
                    &RelPath::parse(&share.path).unwrap_or_default(),
                    mode,
                )
                .is_ok()
            });
            if !creator_ok {
                return rerender(&loaded, Some(ctx.t("share_create.no_rights").into()), false);
            }
            let note = match shares::clean_note(&form.note) {
                Ok(n) => n,
                Err(e) => return rerender(&loaded, Some(share_error(&ctx, &e)), false),
            };
            let Ok(max_file) = parse_mib(&form.max_mib) else {
                return rerender(&loaded, Some(ctx.t("share_create.bad_size").into()), false);
            };
            let expires = match form.expiry.as_str() {
                "keep" | "" => share.expires,
                e => match Expiry::parse(e) {
                    Some(e) => e.until(now()),
                    None => return rerender(&loaded, Some(ctx.t("ops.invalid").into()), false),
                },
            };
            let updated = {
                let (st, id) = (st.clone(), id.clone());
                blocking(move || {
                    st.db.update_share(&id, true, |s| {
                        s.mode = mode.as_str().to_string();
                        s.note = note;
                        s.max_file = max_file.filter(|_| mode.uploads());
                        s.expires = expires;
                    })
                })
                .await
            };
            if !matches!(updated, Some(Ok(Some(_)))) {
                return rerender(&loaded, Some(ctx.t("ops.failed").into()), false);
            }
            audit(
                &st,
                actor,
                client.ip,
                "share_edited",
                &target,
                &format!("{id} {}", mode.as_str()),
            );
            match load(&st, &id).await {
                Some(l) => rerender(&l, None, true),
                None => not_found(),
            }
        }
        "password" => {
            let passphrase = st.words.passphrase(ctx.code());
            let (hash, pp) = Password::Passphrase(passphrase.clone()).hash();
            let done = {
                let (st, id) = (st.clone(), id.clone());
                blocking(move || -> crate::db::Result<()> {
                    st.db.update_share(&id, true, |s| {
                        s.password_hash = hash;
                        s.passphrase = pp;
                    })?;
                    shares::end_guests(&st.db, &shares::share_scope(&id))?;
                    Ok(())
                })
                .await
            };
            if !matches!(done, Some(Ok(()))) {
                return rerender(&loaded, Some(ctx.t("ops.failed").into()), false);
            }
            audit(&st, actor, client.ip, "share_password", &target, &id);
            secret_page(
                &st,
                &ctx,
                ctx.t("share_detail.password_title").to_string(),
                link_of(&st, &share),
                passphrase,
                share.expires,
                (back, ctx.t("share_created.to_details").to_string()),
            )
        }
        "revoke" => {
            if form.confirm != "1" {
                return rerender(
                    &loaded,
                    Some(ctx.t("share_detail.confirm_missing").into()),
                    false,
                );
            }
            let done = {
                let (st, id) = (st.clone(), id.clone());
                blocking(move || -> crate::db::Result<()> {
                    st.db.update_share(&id, true, |s| s.revoked = Some(now()))?;
                    shares::end_guests(&st.db, &shares::share_scope(&id))?;
                    Ok(())
                })
                .await
            };
            if !matches!(done, Some(Ok(()))) {
                return rerender(&loaded, Some(ctx.t("ops.failed").into()), false);
            }
            audit(&st, actor, client.ip, "share_revoked", &target, &id);
            Redirect::to(&back).into_response()
        }
        _ => rerender(&loaded, Some(ctx.t("ops.invalid").into()), false),
    }
}
