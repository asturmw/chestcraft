//! Public uploads. The admin opens uploads into a public volume on `/.cc/admin/volumes`
//! (password generated or picked, open for 1/7/30 days or until closed, optional size
//! limit, optional notification); opening again sets a new password and ends every visitor
//! session. Visitors enter the password at `/.cc/public-write?vol=…` and may then upload
//! and create folders in that volume for 12 hours, never overwrite or delete.

use super::auth::{audit, csrf_ok, forbidden, now, safe_next, same_origin};
use super::files::{Fail, enc, fail};
use super::guest::{self, Unlock};
use super::share_views::{self, PublicState, VolumeRow, VolumesView};
use super::shares::expiry_choices;
use super::views::{AdminPage, Ctx};
use super::{ClientInfo, Current, Shared, page_ctx};
use crate::auth::perm::Principal;
use crate::db::PublicWrite;
use crate::shares::{self, Expiry, Password};
use axum::extract::{Extension, Form, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    tokio::task::spawn_blocking(f).await.ok()
}

/// The open public upload of `vol`, if any.
async fn open_write(st: &Shared, vol: &str) -> Option<PublicWrite> {
    let (st, vol) = (st.clone(), vol.to_string());
    blocking(move || {
        st.db
            .public_write(&vol)
            .ok()
            .flatten()
            .filter(|pw| shares::public_open(&st.cfg, pw, now()))
    })
    .await
    .flatten()
}

fn is_public(st: &Shared, vol: &str) -> bool {
    st.cfg.volumes.get(vol).is_some_and(|v| v.public)
}

#[derive(Deserialize, Default)]
pub struct UnlockQuery {
    #[serde(default)]
    vol: String,
    #[serde(default)]
    next: String,
}

fn next_or_volume(next: &str, vol: &str) -> String {
    if next.is_empty() {
        format!("/{}/", enc(vol))
    } else {
        safe_next(next)
    }
}

pub async fn unlock_page(
    State(st): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<UnlockQuery>,
) -> Response {
    let mut ctx = page_ctx(&st, &headers, &Principal::Anonymous, None);
    ctx.theme = None;
    if !is_public(&st, &q.vol) {
        return fail(&ctx, Fail::NotFound);
    }
    let open = open_write(&st, &q.vol).await.is_some();
    let next = next_or_volume(&q.next, &q.vol);
    share_views::public_unlock(&ctx, &q.vol, &next, open, None).into_response()
}

#[derive(Deserialize, Default)]
pub struct UnlockForm {
    #[serde(default)]
    vol: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    next: String,
}

pub async fn unlock_submit(
    State(st): State<Shared>,
    Extension(client): Extension<ClientInfo>,
    headers: HeaderMap,
    Form(form): Form<UnlockForm>,
) -> Response {
    let ctx = page_ctx(&st, &headers, &Principal::Anonymous, None);
    if !same_origin(&headers, &st.cfg.server.public_url) {
        return forbidden(&ctx);
    }
    if !is_public(&st, &form.vol) {
        return fail(&ctx, Fail::NotFound);
    }
    let next = next_or_volume(&form.next, &form.vol);
    let Some(pw) = open_write(&st, &form.vol).await else {
        return (
            StatusCode::CONFLICT,
            share_views::public_unlock(&ctx, &form.vol, &next, false, None),
        )
            .into_response();
    };
    let key = shares::public_scope(&form.vol);
    let page = |status: StatusCode, msg: String| {
        (
            status,
            share_views::public_unlock(&ctx, &form.vol, &next, true, Some(&msg)),
        )
            .into_response()
    };
    let slowed = |wait: i64| {
        let minutes = ((wait + 59) / 60).max(1);
        ctx.tf("share.slowed_down", &[("minutes", &minutes.to_string())])
    };
    match guest::check_password(
        &st,
        &client,
        &key,
        pw.password_hash.clone(),
        pw.passphrase,
        form.password.clone(),
    )
    .await
    {
        Unlock::Ok => {}
        Unlock::Slowed { wait } => return page(StatusCode::TOO_MANY_REQUESTS, slowed(wait)),
        Unlock::Wrong { wait } => {
            audit(
                &st,
                "anon".into(),
                client.ip,
                "public_unlock_failed",
                &form.vol,
                "",
            );
            let msg = if wait > 0 {
                slowed(wait)
            } else {
                ctx.t("share.wrong").to_string()
            };
            return page(StatusCode::UNAUTHORIZED, msg);
        }
    }
    let Some(cookie) = guest::start(&st, &client, key, shares::PUBLIC_COOKIE, "/").await else {
        return fail(&ctx, Fail::Internal);
    };
    audit(
        &st,
        format!("public:{}", form.vol),
        client.ip,
        "public_unlock",
        &form.vol,
        &client.country_code(),
    );
    let mut res = Redirect::to(&next).into_response();
    if let Ok(v) = HeaderValue::from_str(&cookie) {
        res.headers_mut().insert(header::SET_COOKIE, v);
    }
    res
}

// ---------------------------------------------------------------------------------------
// Admin: volumes

fn volumes_view(
    ctx: &Ctx,
    st: &Shared,
    csrf: &str,
    writes: &[PublicWrite],
    error: Option<String>,
    failed_open: Option<&str>,
) -> VolumesView {
    // The error of a refused "open" goes into that volume's form (when it has one).
    let has_form = |vol: &str| st.cfg.volumes.values().any(|v| v.public && v.name == vol);
    let (error, open_error) = match failed_open.filter(|vol| has_form(vol)) {
        Some(vol) => (None, error.map(|e| (vol, e))),
        None => (error, None),
    };
    let rows = st
        .cfg
        .volumes
        .values()
        .map(|v| {
            let long = |key: &str| Some(ctx.t(key).to_string());
            let mut tags = Vec::new();
            if v.public {
                tags.push((
                    ctx.t("nav.tag_public").to_string(),
                    long("volumes.flag_public"),
                ));
            }
            if !v.remote {
                tags.push((
                    ctx.t("nav.tag_home").to_string(),
                    long("volumes.flag_local"),
                ));
            }
            if v.read_only {
                tags.push((ctx.t("volumes.flag_read_only").to_string(), None));
            }
            for ro in &v.read_only_paths {
                tags.push((
                    ctx.tf("volumes.flag_read_only_path", &[("path", ro.as_str())]),
                    None,
                ));
            }
            if v.stage_folder_uploads {
                tags.push((
                    ctx.t("volumes.tag_staged").to_string(),
                    long("volumes.flag_staged"),
                ));
            }
            let days = v.trash_days(&st.cfg);
            tags.push((
                if days == 0 {
                    ctx.t("volumes.flag_trash_kept").to_string()
                } else {
                    ctx.tn("volumes.flag_trash_days", u64::from(days))
                },
                None,
            ));
            let public = v.public.then(|| {
                let pw = writes
                    .iter()
                    .find(|w| w.volume == v.name)
                    .filter(|w| shares::public_open(&st.cfg, w, now()));
                let state = match pw {
                    Some(w) => match w.until {
                        Some(t) => ctx.tf("volumes.open_until", &[("date", &ctx.datetime(t))]),
                        None => ctx.t("volumes.open_forever").to_string(),
                    },
                    None => ctx.t("volumes.closed").to_string(),
                };
                PublicState {
                    state,
                    open: pw.is_some(),
                    expiries: expiry_choices(ctx, Expiry::Week),
                    error: open_error
                        .as_ref()
                        .filter(|(vol, _)| *vol == v.name)
                        .map(|(_, e)| e.clone()),
                }
            });
            VolumeRow {
                name: v.name.clone(),
                href: format!("/{}/", enc(&v.name)),
                path: v.path.display().to_string(),
                tags,
                public,
            }
        })
        .collect();
    VolumesView {
        csrf: csrf.to_string(),
        rows,
        error,
    }
}

async fn page_data(st: &Shared) -> Vec<PublicWrite> {
    let st = st.clone();
    blocking(move || st.db.public_writes().unwrap_or_default())
        .await
        .unwrap_or_default()
}

pub async fn volumes_page(
    State(st): State<Shared>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
) -> Response {
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    let Some(cu) = current.as_deref().filter(|_| principal.is_admin()) else {
        return fail(&ctx, Fail::NotFound);
    };
    let ctx = ctx.admin_page(AdminPage::Volumes);
    let data = page_data(&st).await;
    share_views::admin_volumes(&ctx, &volumes_view(&ctx, &st, &cu.csrf, &data, None, None))
        .into_response()
}

#[derive(Deserialize, Default)]
pub struct VolumesForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    vol: String,
    #[serde(default)]
    op: String,
    #[serde(default)]
    expiry: String,
    #[serde(default)]
    own_password: String,
    #[serde(default)]
    max_mib: String,
    #[serde(default)]
    notify: String,
}

pub async fn volumes_submit(
    State(st): State<Shared>,
    Extension(client): Extension<ClientInfo>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
    Form(form): Form<VolumesForm>,
) -> Response {
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    let Some(cu) = current.as_deref().filter(|_| principal.is_admin()) else {
        return fail(&ctx, Fail::NotFound);
    };
    let ctx = ctx.admin_page(AdminPage::Volumes);
    if !csrf_ok(&headers, &st, cu, &form.csrf) {
        return forbidden(&ctx);
    }
    // A refused "open" opens its form again with the reason.
    let failed_open = (form.op == "open").then_some(form.vol.as_str());
    let rerender = |data: Vec<PublicWrite>, msg: String| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            share_views::admin_volumes(
                &ctx,
                &volumes_view(&ctx, &st, &cu.csrf, &data, Some(msg), failed_open),
            ),
        )
            .into_response()
    };
    if !is_public(&st, &form.vol) {
        return rerender(page_data(&st).await, ctx.t("volumes.not_public").into());
    }
    let actor = format!("user:{}", cu.key);
    let scope = shares::public_scope(&form.vol);
    match form.op.as_str() {
        "open" => {
            let expiry = Expiry::parse(&form.expiry).unwrap_or(Expiry::Week);
            let own = form.own_password.trim();
            let password = if own.is_empty() {
                Password::Passphrase(st.words.passphrase(ctx.code()))
            } else {
                Password::Own(own.to_string())
            };
            if password.check().is_err() {
                return rerender(
                    page_data(&st).await,
                    ctx.tf(
                        "share_create.password_too_short",
                        &[("min", &shares::MIN_OWN_PASSWORD.to_string())],
                    ),
                );
            }
            let max_mib = form.max_mib.trim();
            let max_file = if max_mib.is_empty() {
                None
            } else {
                match max_mib.parse::<u64>() {
                    Ok(n) if (1..=(1 << 24)).contains(&n) => Some(n << 20),
                    _ => {
                        return rerender(
                            page_data(&st).await,
                            ctx.t("share_create.bad_size").into(),
                        );
                    }
                }
            };
            let now = now();
            let (password_hash, passphrase) = password.hash();
            let pw = PublicWrite {
                volume: form.vol.clone(),
                password_hash,
                passphrase,
                opened: now,
                until: expiry.until(now),
                max_file,
                notify: form.notify == "1",
                by: cu.key.clone(),
            };
            let stored = {
                let (st, pw, scope) = (st.clone(), pw.clone(), scope.clone());
                blocking(move || -> crate::db::Result<()> {
                    st.db.put_public_write(&pw)?;
                    shares::end_guests(&st.db, &scope)?;
                    Ok(())
                })
                .await
            };
            if !matches!(stored, Some(Ok(()))) {
                return rerender(page_data(&st).await, ctx.t("ops.failed").into());
            }
            audit(
                &st,
                actor,
                client.ip,
                "public_write_opened",
                &form.vol,
                &format!(
                    "{}{}",
                    expiry.as_str(),
                    if pw.notify { ", notify" } else { "" }
                ),
            );
            let link = format!("{}/{}/", st.cfg.server.public_url, enc(&form.vol));
            secret(
                &ctx,
                ctx.tf("volumes.opened_title", &[("volume", &form.vol)]),
                link,
                password.text().to_string(),
                pw.until,
            )
        }
        "close" => {
            let done = {
                let (st, vol) = (st.clone(), form.vol.clone());
                blocking(move || -> crate::db::Result<()> {
                    st.db.delete_public_write(&vol)?;
                    shares::end_guests(&st.db, &scope)?;
                    Ok(())
                })
                .await
            };
            if !matches!(done, Some(Ok(()))) {
                return rerender(page_data(&st).await, ctx.t("ops.failed").into());
            }
            audit(&st, actor, client.ip, "public_write_closed", &form.vol, "");
            Redirect::to("/.cc/admin/volumes").into_response()
        }
        _ => rerender(page_data(&st).await, ctx.t("ops.invalid").into()),
    }
}

/// The new public upload password, shown once.
fn secret(
    ctx: &Ctx,
    title: String,
    link: String,
    password: String,
    until: Option<i64>,
) -> Response {
    let mut ctx2 = ctx.clone();
    ctx2.lang_switch = None;
    ctx2.scripts.push("copy.js");
    let view = share_views::SecretView {
        title,
        warning: ctx.t("volumes.once").to_string(),
        until: until
            .map(|t| ctx.datetime(t))
            .unwrap_or_else(|| ctx.t("share_expiry.never").to_string()),
        link,
        password,
        messages: Vec::new(),
        back_href: "/.cc/admin/volumes".into(),
        back_label: ctx.t("volumes.back").into(),
    };
    let mut res = share_views::secret(&ctx2, &view).into_response();
    // The password must not stay in the browser's cache.
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}
