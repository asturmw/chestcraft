//! The trash page: what the user deleted (the admin sees everything), restore, and the
//! admin's "empty trash now" per volume.

use super::api;
use super::auth::{audit, csrf_ok, forbidden, now};
use super::views::{self, Ctx, TrashGroup, TrashRow, TrashView};
use super::{ClientInfo, Current, Shared, page_ctx};
use crate::auth::perm::{Perms, Principal, path_perms, volume_perms};
use crate::db::TrashItem;
use crate::fs::path::RelPath;
use crate::fs::trash::{self, REASON_REPLACED, REASON_UNFINISHED};
use axum::extract::{Extension, Form, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use serde::Deserialize;
use std::collections::BTreeMap;

const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

const SIGN_IN: &str = "/.cc/login?next=%2F.cc%2Ftrash";

/// May this principal see (and restore) the item?
fn visible(st: &Shared, principal: &Principal, client: &ClientInfo, item: &TrashItem) -> bool {
    let own = match principal.name() {
        Some(name) => item.deleted_by == format!("user:{}", crate::db::User::key(name)),
        None => false,
    };
    (principal.is_admin() || own)
        && volume_perms(&st.cfg, principal, client.origin, &item.volume).contains(Perms::READ)
}

fn row(st: &Shared, ctx: &Ctx, item: &TrashItem, now: i64) -> TrashRow {
    let rel = RelPath::parse(&item.path).unwrap_or_default();
    let parent = rel.parent().unwrap_or_default();
    let mut href = format!("/{}/", utf8_percent_encode(&item.volume, SEGMENT));
    if !parent.is_root() {
        href.push_str(&parent.to_url());
        href.push('/');
    }
    let until = match trash::purge_time(&st.cfg, item) {
        None => ctx.t("trash.kept").to_string(),
        Some(t) => {
            let days = ((t - now).max(0) + 86_399) / 86_400;
            if days == 0 {
                ctx.t("trash.next_cleanup").to_string()
            } else {
                ctx.tn("trash.days_left", days as u64)
            }
        }
    };
    let size = if item.is_dir {
        ctx.tnf(
            "trash.folder_size",
            item.files,
            &[("size", &ctx.size(item.size))],
        )
    } else {
        ctx.size(item.size)
    };
    TrashRow {
        id: item.id.clone(),
        name: rel.name().unwrap_or(&item.path).to_string(),
        is_dir: item.is_dir,
        location: format!(
            "/{}{}",
            item.volume,
            if parent.is_root() {
                "/".to_string()
            } else {
                format!("{parent}/")
            }
        ),
        location_href: href,
        deleted: ctx.datetime(item.deleted_at),
        by: item
            .deleted_by
            .strip_prefix("user:")
            .unwrap_or(&item.deleted_by)
            .to_string(),
        size,
        until,
        note: match item.reason.as_str() {
            REASON_REPLACED => Some(ctx.t("trash.reason_replaced").to_string()),
            REASON_UNFINISHED => Some(ctx.t("trash.reason_unfinished").to_string()),
            _ => None,
        },
    }
}

pub async fn page(
    State(st): State<Shared>,
    Extension(client): Extension<ClientInfo>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
) -> Response {
    let Some(cu) = current.clone() else {
        return Redirect::to(SIGN_IN).into_response();
    };
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    let items = {
        let st = st.clone();
        tokio::task::spawn_blocking(move || st.db.trash_items()).await
    };
    let Ok(Ok(mut items)) = items else {
        tracing::error!("can't read the trash index");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            views::error_page(
                &ctx,
                ctx.t("error.server_error_title"),
                ctx.t("error.server_error_body"),
            ),
        )
            .into_response();
    };
    items.retain(|i| visible(&st, &principal, &client, i));
    items.sort_by_key(|i| std::cmp::Reverse(i.deleted_at));
    let now = now();
    let mut groups: BTreeMap<String, Vec<&TrashItem>> = BTreeMap::new();
    for item in &items {
        groups.entry(item.volume.clone()).or_default().push(item);
    }
    let view = TrashView {
        admin: principal.is_admin(),
        csrf: cu.csrf.clone(),
        groups: groups
            .into_iter()
            .map(|(volume, items)| TrashGroup {
                summary: ctx.tnf(
                    "trash.summary",
                    items.len() as u64,
                    &[("size", &ctx.size(items.iter().map(|i| i.size).sum()))],
                ),
                rows: items.iter().map(|i| row(&st, &ctx, i, now)).collect(),
                volume,
            })
            .collect(),
    };
    views::trash(&ctx, &view).into_response()
}

fn problem(ctx: &Ctx, status: StatusCode, body: &str) -> Response {
    (
        status,
        views::problems(
            ctx,
            ctx.t("ops.failed_title"),
            body,
            &[],
            Some("/.cc/trash"),
        ),
    )
        .into_response()
}

#[derive(Deserialize)]
pub struct RestoreForm {
    id: String,
    #[serde(default)]
    csrf: String,
}

pub async fn restore(
    State(st): State<Shared>,
    Extension(client): Extension<ClientInfo>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
    Form(form): Form<RestoreForm>,
) -> Response {
    let Some(cu) = current.clone() else {
        return Redirect::to(SIGN_IN).into_response();
    };
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    if !csrf_ok(&headers, &st, &cu, &form.csrf) {
        return forbidden(&ctx);
    }
    let item = {
        let st = st.clone();
        tokio::task::spawn_blocking(move || st.db.trash_item(&form.id)).await
    };
    let item = match item {
        Ok(Ok(Some(i))) if visible(&st, &principal, &client, &i) => i,
        Ok(Ok(_)) => return problem(&ctx, StatusCode::NOT_FOUND, ctx.t("trash.gone")),
        _ => return problem(&ctx, StatusCode::INTERNAL_SERVER_ERROR, ctx.t("ops.failed")),
    };
    let rel = RelPath::parse(&item.path).unwrap_or_default();
    let name = rel.name().unwrap_or_default().to_string();
    let Some(vol) = st.volumes.get(&item.volume).cloned() else {
        return problem(&ctx, StatusCode::NOT_FOUND, ctx.t("trash.gone"));
    };
    if !path_perms(&st.cfg, &principal, client.origin, vol.name(), &rel).contains(Perms::WRITE) {
        return problem(
            &ctx,
            StatusCode::FORBIDDEN,
            &ctx.tf("trash.no_write", &[("name", &name)]),
        );
    }
    let restored = {
        let (st, item) = (st.clone(), item.clone());
        tokio::task::spawn_blocking(move || trash::restore(&st.db, &vol, &item)).await
    };
    match restored {
        Ok(Ok(path)) => {
            audit(
                &st,
                format!("user:{}", cu.key),
                client.ip,
                "trash_restored",
                &format!("{}/{}", item.volume, path.as_str()),
                &item.path,
            );
            Redirect::to("/.cc/trash").into_response()
        }
        Ok(Err(e)) => {
            let (status, _, msg) = api::message(&ctx, &e, &name);
            let msg = if matches!(e, crate::fs::ops::OpError::Gone) {
                ctx.t("trash.gone").to_string()
            } else {
                msg
            };
            problem(&ctx, status, &msg)
        }
        Err(_) => problem(&ctx, StatusCode::INTERNAL_SERVER_ERROR, ctx.t("ops.failed")),
    }
}

#[derive(Deserialize)]
pub struct EmptyForm {
    vol: String,
    #[serde(default)]
    confirm: String,
    #[serde(default)]
    csrf: String,
}

pub async fn empty(
    State(st): State<Shared>,
    Extension(client): Extension<ClientInfo>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
    Form(form): Form<EmptyForm>,
) -> Response {
    let Some(cu) = current.clone() else {
        return Redirect::to(SIGN_IN).into_response();
    };
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    if !csrf_ok(&headers, &st, &cu, &form.csrf) {
        return forbidden(&ctx);
    }
    if !principal.is_admin()
        || !volume_perms(&st.cfg, &principal, client.origin, &form.vol).contains(Perms::READ)
    {
        return problem(&ctx, StatusCode::NOT_FOUND, ctx.t("error.not_found_body"));
    }
    if form.confirm != "1" {
        return problem(
            &ctx,
            StatusCode::BAD_REQUEST,
            ctx.t("trash.confirm_missing"),
        );
    }
    let actor = format!("user:{}", cu.key);
    let result = {
        let (st, vol, actor) = (st.clone(), form.vol.clone(), actor.clone());
        tokio::task::spawn_blocking(move || {
            let items: Vec<TrashItem> = st
                .db
                .trash_items()?
                .into_iter()
                .filter(|i| i.volume == vol)
                .collect();
            Ok::<_, crate::db::DbError>(trash::purge_items(
                &st.db,
                &st.volumes,
                &items,
                &actor,
                now(),
            ))
        })
        .await
    };
    match result {
        Ok(Ok((purged, failed))) => {
            audit(
                &st,
                actor,
                client.ip,
                "trash_emptied",
                &form.vol,
                &format!("{purged} items purged, {failed} failed"),
            );
            if failed > 0 {
                return problem(
                    &ctx,
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &ctx.tn("trash.empty_failed", failed as u64),
                );
            }
            Redirect::to("/.cc/trash").into_response()
        }
        _ => problem(&ctx, StatusCode::INTERNAL_SERVER_ERROR, ctx.t("ops.failed")),
    }
}
