//! Access management on the web: the admin's accounts page (people, agents and their MCP
//! tokens) and audit log, plus the access parts of the own account page: WebDAV app
//! passwords and signing out on other devices. The markup is provisional until the design
//! session delivers these screens.
//!
//! Secrets (one-time passwords, app passwords, tokens) appear once, on a page sent with
//! `Cache-Control: no-store`, never in a URL. Every change is audited with the admin as
//! actor. An admin can't block themselves or drop their own admin role here.

use super::auth::{audit, csrf_ok, forbidden, now};
use super::files::{Fail, enc, fail};
use super::query::Query;
use super::views::{Ctx, layout};
use super::{AppState, ClientInfo, Current, CurrentUser, Shared, page_ctx};
use crate::auth::accounts::{self, AccountError};
use crate::auth::agent::{self, AgentError};
use crate::auth::apppass;
use crate::auth::perm::{ADMIN_ROLE, Principal};
use crate::db::{AgentToken, AppPassword, AuditEvent, Session, User};
use axum::extract::{Extension, Form, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Redirect, Response};
use maud::{Markup, html};
use serde::Deserialize;

const ACCOUNT: &str = "/.cc/account";
const USERS: &str = "/.cc/admin/users";
/// Audit entries searched per page view, and shown.
const AUDIT_SCAN: usize = 5000;
const AUDIT_SHOW: usize = 200;
/// Expiry choices for new MCP tokens, in days (0 = never).
const TOKEN_DAYS: [u32; 4] = [30, 90, 365, 0];

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    tokio::task::spawn_blocking(f).await.ok()
}

fn no_store(mut res: Response) -> Response {
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

fn actor(cu: &CurrentUser) -> String {
    format!("user:{}", cu.key)
}

/// One secret, shown once, with a copy button.
fn secret_page(
    ctx: &Ctx,
    title: &str,
    rows: &[(&str, &str)],
    note: &str,
    back: (&str, &str),
) -> Response {
    let mut ctx = ctx.clone();
    ctx.scripts.push("copy.js");
    let body = html! {
        h1 class="share-secret__title" { (title) }
        p class="banner" data-state="warning" role="status" { (note) }
        dl class="share-secret" {
            @for (i, (label, value)) in rows.iter().enumerate() {
                dt { (label) }
                dd {
                    input class="field__input" id=(format!("secret-{i}")) type="text" readonly value=(value);
                    " " button type="button" data-js="copy" data-copy=(format!("secret-{i}")) { (ctx.t("access.copy")) }
                }
            }
        }
        p { a href=(back.0) { (back.1) } }
    };
    no_store(layout(&ctx, title, body).into_response())
}

// ---- own account: app passwords and sessions ----------------------------------------------

pub struct AccessView {
    pub app_passwords: Vec<AppPassword>,
    /// (this browser, session), newest first.
    pub sessions: Vec<(bool, Session)>,
}

pub async fn access_view(st: &Shared, cu: &CurrentUser) -> AccessView {
    let (st, key, me) = (st.clone(), cu.key.clone(), cu.session_hash.clone());
    blocking(move || {
        let mut app_passwords: Vec<AppPassword> = st
            .db
            .app_passwords_of(&key)
            .unwrap_or_default()
            .into_iter()
            .map(|(_, r)| r)
            .collect();
        app_passwords.sort_by_key(|r| r.created);
        let mut sessions: Vec<(bool, Session)> = st
            .db
            .sessions_of(&key)
            .unwrap_or_default()
            .into_iter()
            .map(|(id, s)| (id == me, s))
            .collect();
        sessions.sort_by_key(|(_, s)| std::cmp::Reverse(s.last_seen));
        AccessView {
            app_passwords,
            sessions,
        }
    })
    .await
    .unwrap_or(AccessView {
        app_passwords: Vec::new(),
        sessions: Vec::new(),
    })
}

fn used(ctx: &Ctx, at: Option<i64>, ip: Option<&str>) -> String {
    match (at, ip) {
        (Some(t), Some(ip)) => ctx.tf(
            "access.used_from",
            &[("time", &ctx.datetime(t)), ("ip", ip)],
        ),
        (Some(t), None) => ctx.tf("access.used", &[("time", &ctx.datetime(t))]),
        _ => ctx.t("access.never_used").to_string(),
    }
}

/// The app password and session sections of the account page. `q` carries the outcome of
/// the last form (`app=revoked|<error>`, `signed_out=<n>`).
pub fn access_sections(ctx: &Ctx, csrf: &str, v: &AccessView, q: &Query) -> Markup {
    let app_msg = q.get("app").map(|code| match code {
        "revoked" => ("ok", ctx.t("access.app_revoked").to_string()),
        "label" => (
            "error",
            ctx.tf(
                "access.bad_label",
                &[("max", &apppass::MAX_LABEL_CHARS.to_string())],
            ),
        ),
        "duplicate" => ("error", ctx.t("access.app_duplicate").to_string()),
        "too_many" => (
            "error",
            ctx.tf(
                "access.app_too_many",
                &[("max", &apppass::MAX_PER_USER.to_string())],
            ),
        ),
        _ => ("error", ctx.t("ops.failed").to_string()),
    });
    let others = v.sessions.iter().filter(|(me, _)| !me).count() as u64;
    html! {
        section class="account__app-passwords" id="app-passwords" aria-labelledby="app-passwords-title" {
            h2 id="app-passwords-title" { (ctx.t("access.app_passwords")) }
            p { (ctx.t("access.app_intro")) }
            @if let Some((state, text)) = &app_msg {
                p class="banner" data-state=(state) role="status" { (text) }
            }
            @if v.app_passwords.is_empty() {
                p class="account__empty" { (ctx.t("access.app_none")) }
            } @else {
                ul class="token-list" {
                    @for r in &v.app_passwords {
                        li class="token-list__item" {
                            strong { (r.label) }
                            " · " (ctx.tf("access.created", &[("time", &ctx.datetime(r.created))]))
                            " · " (used(ctx, r.last_used, r.last_ip.as_deref()))
                            form class="token-list__revoke" method="post" action="/.cc/account/app-passwords" {
                                input type="hidden" name="csrf" value=(csrf);
                                input type="hidden" name="op" value="revoke";
                                input type="hidden" name="id" value=(r.id);
                                button type="submit" { (ctx.t("access.revoke")) }
                            }
                        }
                    }
                }
            }
            form class="token-add" method="post" action="/.cc/account/app-passwords" {
                input type="hidden" name="csrf" value=(csrf);
                input type="hidden" name="op" value="add";
                label class="field" {
                    span class="field__label" { (ctx.t("access.device")) }
                    input class="field__input" type="text" name="label" required
                        maxlength=(apppass::MAX_LABEL_CHARS) placeholder=(ctx.t("access.device_hint"));
                }
                button type="submit" { (ctx.t("access.app_add")) }
            }
        }
        section class="account__sessions" id="sessions" aria-labelledby="sessions-title" {
            h2 id="sessions-title" { (ctx.t("access.sessions")) }
            @if let Some(n) = q.get("signed_out").and_then(|n| n.parse::<u64>().ok()) {
                p class="banner" data-state="ok" role="status" { (ctx.tn("access.signed_out", n)) }
            }
            ul class="session-list" {
                @for (me, s) in &v.sessions {
                    li class="session-list__item" data-state=(if *me { "current" } else { "other" }) {
                        (ctx.tf("access.session", &[("time", &ctx.datetime(s.last_seen)), ("ip", &s.ip)]))
                        @if *me { " · " (ctx.t("access.this_device")) }
                    }
                }
            }
            @if others == 0 {
                p { (ctx.t("access.only_here")) }
            } @else {
                form method="post" action="/.cc/account/sessions" {
                    input type="hidden" name="csrf" value=(csrf);
                    button type="submit" { (ctx.tn("access.sign_out_others", others)) }
                }
            }
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct AppPasswordForm {
    csrf: String,
    op: String,
    label: String,
    id: String,
}

/// POST `/.cc/account/app-passwords`: create (shown once) or revoke an own app password.
pub async fn app_passwords_submit(
    State(st): State<Shared>,
    Extension(client): Extension<ClientInfo>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
    Form(form): Form<AppPasswordForm>,
) -> Response {
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    let Some(cu) = current.as_deref() else {
        return Redirect::to("/.cc/login").into_response();
    };
    if !csrf_ok(&headers, &st, cu, &form.csrf) {
        return forbidden(&ctx);
    }
    let back =
        |code: &str| Redirect::to(&format!("{ACCOUNT}?app={code}#app-passwords")).into_response();
    match form.op.as_str() {
        "add" => {
            let (st2, name, label) = (st.clone(), cu.name.clone(), form.label.clone());
            match blocking(move || apppass::create(&st2.db, &name, &label, now())).await {
                Some(Ok((secret, rec))) => {
                    audit(
                        &st,
                        actor(cu),
                        client.ip,
                        "app_password_created",
                        &cu.key,
                        &format!("{} {}", rec.id, rec.label),
                    );
                    secret_page(
                        &ctx,
                        &ctx.tf("access.app_created", &[("label", &rec.label)]),
                        &[
                            (ctx.t("access.user_name"), &cu.name),
                            (ctx.t("access.app_password"), &secret),
                        ],
                        ctx.t("access.app_once"),
                        (
                            &format!("{ACCOUNT}#app-passwords"),
                            ctx.t("access.back_account"),
                        ),
                    )
                }
                Some(Err(apppass::CreateError::BadLabel)) => back("label"),
                Some(Err(apppass::CreateError::Duplicate)) => back("duplicate"),
                Some(Err(apppass::CreateError::TooMany)) => back("too_many"),
                _ => back("failed"),
            }
        }
        "revoke" => {
            let (st2, key, id) = (st.clone(), cu.key.clone(), form.id.clone());
            match blocking(move || apppass::revoke(&st2.db, &key, &id)).await {
                Some(Ok(Some(r))) => {
                    audit(
                        &st,
                        actor(cu),
                        client.ip,
                        "app_password_revoked",
                        &cu.key,
                        &format!("{} {}", r.id, r.label),
                    );
                    back("revoked")
                }
                _ => back("failed"),
            }
        }
        _ => fail(&ctx, Fail::BadRequest),
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct CsrfForm {
    csrf: String,
}

/// POST `/.cc/account/sessions`: end every session of this account except this one.
pub async fn sessions_submit(
    State(st): State<Shared>,
    Extension(client): Extension<ClientInfo>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
    Form(form): Form<CsrfForm>,
) -> Response {
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    let Some(cu) = current.as_deref() else {
        return Redirect::to("/.cc/login").into_response();
    };
    if !csrf_ok(&headers, &st, cu, &form.csrf) {
        return forbidden(&ctx);
    }
    let (st2, key, me) = (st.clone(), cu.key.clone(), cu.session_hash.clone());
    let Some(Ok(n)) = blocking(move || st2.db.delete_user_sessions(&key, Some(&me))).await else {
        return fail(&ctx, Fail::Internal);
    };
    audit(
        &st,
        actor(cu),
        client.ip,
        "sessions_ended",
        &cu.key,
        &n.to_string(),
    );
    Redirect::to(&format!("{ACCOUNT}?signed_out={n}#sessions")).into_response()
}

// ---- admin: accounts ------------------------------------------------------------------

pub struct UsersData {
    pub people: Vec<User>,
    pub agents: Vec<(User, Vec<AgentToken>)>,
    /// "admin" and the roles of the config.
    pub roles: Vec<String>,
    pub mcp_on: bool,
}

async fn users_data(st: &Shared) -> UsersData {
    let st = st.clone();
    blocking(move || {
        let mut users = st.db.users().unwrap_or_default();
        users.sort_by_key(|u| u.name.to_lowercase());
        let tokens = st.db.agent_tokens().unwrap_or_default();
        let (agents, people): (Vec<User>, Vec<User>) = users.into_iter().partition(User::is_agent);
        let agents = agents
            .into_iter()
            .map(|a| {
                let key = User::key(&a.name);
                let mut own: Vec<AgentToken> = tokens
                    .iter()
                    .filter(|(_, t)| t.agent == key)
                    .map(|(_, t)| t.clone())
                    .collect();
                own.sort_by_key(|t| t.created);
                (a, own)
            })
            .collect();
        let mut roles = vec![ADMIN_ROLE.to_string()];
        roles.extend(st.cfg.roles.keys().cloned());
        UsersData {
            people,
            agents,
            roles,
            mcp_on: st.cfg.mcp.listen.is_some(),
        }
    })
    .await
    .unwrap_or(UsersData {
        people: Vec::new(),
        agents: Vec::new(),
        roles: Vec::new(),
        mcp_on: false,
    })
}

fn role_select(name: &str, roles: &[String], current: &str, agents: bool) -> Markup {
    html! {
        select class="field__input" name=(name) {
            @for r in roles.iter().filter(|r| !agents || *r != ADMIN_ROLE) {
                option value=(r) selected[r == current] { (r) }
            }
        }
    }
}

fn hidden(csrf: &str, op: &str, name: &str) -> Markup {
    html! {
        input type="hidden" name="csrf" value=(csrf);
        input type="hidden" name="op" value=(op);
        input type="hidden" name="name" value=(name);
    }
}

fn users_view(
    ctx: &Ctx,
    st: &AppState,
    cu: &CurrentUser,
    d: &UsersData,
    msg: Option<(&str, String)>,
) -> Markup {
    let title = ctx.t("users.title");
    let csrf = cu.csrf.as_str();
    let role_ok = |r: &str| accounts::role_exists(&st.cfg, r);
    layout(
        ctx,
        title,
        html! {
            h1 class="users__title" { (title) }
            p { a href="/.cc/admin/audit" { (ctx.t("users.audit_link")) } }
            @if let Some((state, text)) = &msg {
                p class="banner" data-state=(state) role="status" { (text) }
            }
            section class="users__people" aria-labelledby="people-title" {
                h2 id="people-title" { (ctx.t("users.people")) }
                table class="users-table" {
                    thead { tr {
                        th scope="col" { (ctx.t("users.name")) }
                        th scope="col" { (ctx.t("users.role")) }
                        th scope="col" { (ctx.t("users.last_login")) }
                        th scope="col" { (ctx.t("users.actions")) }
                    } }
                    tbody {
                        @for u in &d.people {
                            @let me = User::key(&u.name) == cu.key;
                            tr data-state=(if u.disabled { "disabled" } else { "active" }) {
                                th scope="row" {
                                    (u.name)
                                    @if u.disabled { " · " span class="badge" { (ctx.t("users.blocked")) } }
                                    @if u.must_change_password { " · " span class="badge" { (ctx.t("users.must_change")) } }
                                    @if !role_ok(&u.role) { " · " span class="badge" data-state="warning" { (ctx.t("users.role_missing")) } }
                                }
                                td {
                                    @if me {
                                        (u.role)
                                    } @else {
                                        form class="users__role" method="post" action=(USERS) {
                                            (hidden(csrf, "role", &u.name))
                                            (role_select("role", &d.roles, &u.role, false))
                                            " " button type="submit" { (ctx.t("users.set_role")) }
                                        }
                                    }
                                }
                                td { @match u.last_login { Some(t) => (ctx.datetime(t)), None => (ctx.t("users.never")) } }
                                td class="users__actions" {
                                    form method="post" action=(USERS) {
                                        (hidden(csrf, "passwd", &u.name))
                                        button type="submit" { (ctx.t("users.passwd")) }
                                    }
                                    @if !me {
                                        form method="post" action=(USERS) {
                                            (hidden(csrf, if u.disabled { "enable" } else { "disable" }, &u.name))
                                            button type="submit" { (ctx.t(if u.disabled { "users.enable" } else { "users.disable" })) }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                form class="users__add" method="post" action=(USERS) {
                    input type="hidden" name="csrf" value=(csrf);
                    input type="hidden" name="op" value="add";
                    h3 { (ctx.t("users.add_title")) }
                    label class="field" {
                        span class="field__label" { (ctx.t("users.name")) }
                        input class="field__input" type="text" name="name" required maxlength="32" autocomplete="off";
                    }
                    label class="field" {
                        span class="field__label" { (ctx.t("users.role")) }
                        (role_select("role", &d.roles, d.roles.get(1).map(String::as_str).unwrap_or(ADMIN_ROLE), false))
                    }
                    button type="submit" { (ctx.t("users.add")) }
                }
            }
            section class="users__agents" aria-labelledby="agents-title" {
                h2 id="agents-title" { (ctx.t("users.agents")) }
                p { (ctx.t("users.agents_intro")) }
                @if !d.mcp_on { p class="banner" data-state="warning" { (ctx.t("users.mcp_off")) } }
                @for (a, tokens) in &d.agents {
                    article class="agent-card" data-state=(if a.disabled { "disabled" } else { "active" }) {
                        h3 class="agent-card__name" {
                            (a.name)
                            span class="badge" { (ctx.t("users.agent_badge")) }
                            @if a.disabled { " " span class="badge" { (ctx.t("users.blocked")) } }
                            @if !role_ok(&a.role) { " " span class="badge" data-state="warning" { (ctx.t("users.role_missing")) } }
                        }
                        form class="users__role" method="post" action=(USERS) {
                            (hidden(csrf, "role", &a.name))
                            (role_select("role", &d.roles, &a.role, true))
                            " " button type="submit" { (ctx.t("users.set_role")) }
                        }
                        form method="post" action=(USERS) {
                            (hidden(csrf, if a.disabled { "enable" } else { "disable" }, &a.name))
                            button type="submit" { (ctx.t(if a.disabled { "users.enable" } else { "users.disable" })) }
                        }
                        @if tokens.is_empty() {
                            p { (ctx.t("users.tokens_none")) }
                        } @else {
                            ul class="token-list" {
                                @for t in tokens {
                                    li class="token-list__item" data-state=(if t.expires.is_some_and(|e| e <= now()) { "expired" } else { "active" }) {
                                        strong { (t.label) }
                                        " · " (ctx.tf("access.created", &[("time", &ctx.datetime(t.created))]))
                                        @if let Some(e) = t.expires {
                                            " · " (ctx.tf("users.expires_at", &[("time", &ctx.datetime(e))]))
                                        }
                                        " · " (used(ctx, t.last_used, t.last_ip.as_deref()))
                                        form class="token-list__revoke" method="post" action=(USERS) {
                                            (hidden(csrf, "agent_revoke", &a.name))
                                            input type="hidden" name="id" value=(t.id);
                                            button type="submit" { (ctx.t("access.revoke")) }
                                        }
                                    }
                                }
                            }
                        }
                        form class="token-add" method="post" action=(USERS) {
                            (hidden(csrf, "agent_token", &a.name))
                            label class="field" {
                                span class="field__label" { (ctx.t("users.token_host")) }
                                input class="field__input" type="text" name="label" required maxlength=(agent::MAX_LABEL_CHARS);
                            }
                            label class="field" {
                                span class="field__label" { (ctx.t("users.token_expires")) }
                                select class="field__input" name="expires" {
                                    @for days in TOKEN_DAYS {
                                        option value=(days) selected[days == 90] {
                                            @if days == 0 { (ctx.t("users.expires_never")) }
                                            @else { (ctx.tn("users.expires_days", u64::from(days))) }
                                        }
                                    }
                                }
                            }
                            button type="submit" { (ctx.t("users.token_add")) }
                        }
                    }
                }
                form class="users__add" method="post" action=(USERS) {
                    input type="hidden" name="csrf" value=(csrf);
                    input type="hidden" name="op" value="agent_add";
                    h3 { (ctx.t("users.agent_add_title")) }
                    label class="field" {
                        span class="field__label" { (ctx.t("users.name")) }
                        input class="field__input" type="text" name="name" required maxlength="32" autocomplete="off";
                    }
                    label class="field" {
                        span class="field__label" { (ctx.t("users.role")) }
                        (role_select("role", &d.roles, "", true))
                    }
                    button type="submit" { (ctx.t("users.agent_add")) }
                }
            }
        },
    )
}

fn admin_of<'a>(principal: &Principal, current: &'a Current) -> Option<&'a CurrentUser> {
    current.as_deref().filter(|_| principal.is_admin())
}

/// GET `/.cc/admin/users`.
pub async fn users_page(
    State(st): State<Shared>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    let Some(cu) = admin_of(&principal, &current) else {
        return fail(&ctx, Fail::NotFound);
    };
    let q = Query::parse(uri.query());
    let msg = q
        .has("saved")
        .then(|| ("ok", ctx.t("users.saved").to_string()));
    let d = users_data(&st).await;
    users_view(&ctx, &st, cu, &d, msg).into_response()
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct UsersForm {
    csrf: String,
    op: String,
    name: String,
    role: String,
    label: String,
    expires: String,
    id: String,
}

fn account_error_text(ctx: &Ctx, e: &AccountError) -> String {
    match e {
        AccountError::BadName(n) => ctx.tf("users.bad_name", &[("name", n)]),
        AccountError::UnknownRole(r) => ctx.tf("users.unknown_role", &[("role", r)]),
        AccountError::Exists(n) => ctx.tf("users.exists", &[("name", n)]),
        AccountError::NoAccount(n) => ctx.tf("users.no_account", &[("name", n)]),
        AccountError::IsAgent(n) => ctx.tf("users.is_agent", &[("name", n)]),
        AccountError::AgentAdmin(n) => ctx.tf("users.agent_admin", &[("name", n)]),
        AccountError::Db(_) => ctx.t("ops.failed").to_string(),
    }
}

fn agent_error_text(ctx: &Ctx, e: &AgentError) -> String {
    match e {
        AgentError::BadName(n) => ctx.tf("users.bad_name", &[("name", n)]),
        AgentError::AdminRole => ctx.t("users.agent_admin_role").to_string(),
        AgentError::UnknownRole(r) => ctx.tf("users.unknown_role", &[("role", r)]),
        AgentError::Exists(n) => ctx.tf("users.exists", &[("name", n)]),
        AgentError::NoAgent(n) => ctx.tf("users.no_account", &[("name", n)]),
        AgentError::BadLabel => ctx.tf(
            "access.bad_label",
            &[("max", &agent::MAX_LABEL_CHARS.to_string())],
        ),
        AgentError::Duplicate => ctx.t("users.token_duplicate").to_string(),
        AgentError::TooMany => ctx.tf(
            "users.token_too_many",
            &[("max", &agent::MAX_TOKENS_PER_AGENT.to_string())],
        ),
        AgentError::Expired => ctx.t("ops.failed").to_string(),
        AgentError::Db(_) => ctx.t("ops.failed").to_string(),
    }
}

/// POST `/.cc/admin/users`.
pub async fn users_submit(
    State(st): State<Shared>,
    Extension(client): Extension<ClientInfo>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
    Form(form): Form<UsersForm>,
) -> Response {
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    let Some(cu) = admin_of(&principal, &current) else {
        return fail(&ctx, Fail::NotFound);
    };
    if !csrf_ok(&headers, &st, cu, &form.csrf) {
        return forbidden(&ctx);
    }
    let error = |text: String| {
        let (st, ctx, cu) = (st.clone(), ctx.clone(), cu.clone());
        async move {
            let d = users_data(&st).await;
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                users_view(&ctx, &st, &cu, &d, Some(("error", text))),
            )
                .into_response()
        }
    };
    let saved = || Redirect::to(&format!("{USERS}?saved")).into_response();
    // The operations trim the name, so the guard must too.
    let me = User::key(form.name.trim()) == cu.key;
    let back = (USERS, ctx.t("users.back"));
    let db = st.db.clone();
    let (name, role) = (form.name.trim().to_string(), form.role.clone());
    match form.op.as_str() {
        "add" => {
            let cfg = st.cfg.clone();
            let r = blocking(move || accounts::create_person(&db, &cfg, &name, &role, now())).await;
            match r {
                Some(Ok((u, pw))) => {
                    audit(
                        &st,
                        actor(cu),
                        client.ip,
                        "user_created",
                        &User::key(&u.name),
                        &u.role,
                    );
                    secret_page(
                        &ctx,
                        &ctx.tf("users.created", &[("name", &u.name)]),
                        &[
                            (ctx.t("access.user_name"), &u.name),
                            (ctx.t("users.one_time"), &pw),
                        ],
                        &ctx.tf("users.one_time_note", &[("name", &u.name)]),
                        back,
                    )
                }
                Some(Err(e)) => error(account_error_text(&ctx, &e)).await,
                None => error(ctx.t("ops.failed").into()).await,
            }
        }
        "passwd" => match blocking(move || accounts::reset_password(&db, &name)).await {
            Some(Ok((u, pw))) => {
                audit(
                    &st,
                    actor(cu),
                    client.ip,
                    "user_password_reset",
                    &User::key(&u.name),
                    "",
                );
                secret_page(
                    &ctx,
                    &ctx.tf("users.passwd_title", &[("name", &u.name)]),
                    &[
                        (ctx.t("access.user_name"), &u.name),
                        (ctx.t("users.one_time"), &pw),
                    ],
                    &ctx.tf("users.one_time_note", &[("name", &u.name)]),
                    back,
                )
            }
            Some(Err(e)) => error(account_error_text(&ctx, &e)).await,
            None => error(ctx.t("ops.failed").into()).await,
        },
        "role" if me => error(ctx.t("users.not_yourself").into()).await,
        "role" => {
            let cfg = st.cfg.clone();
            match blocking(move || accounts::set_role(&db, &cfg, &name, &role)).await {
                Some(Ok(u)) => {
                    audit(
                        &st,
                        actor(cu),
                        client.ip,
                        "user_role",
                        &User::key(&u.name),
                        &u.role,
                    );
                    saved()
                }
                Some(Err(e)) => error(account_error_text(&ctx, &e)).await,
                None => error(ctx.t("ops.failed").into()).await,
            }
        }
        "disable" | "enable" if me => error(ctx.t("users.not_yourself").into()).await,
        op @ ("disable" | "enable") => {
            let off = op == "disable";
            match blocking(move || accounts::set_disabled(&db, &name, off)).await {
                Some(Ok((u, _))) => {
                    let action = match (u.is_agent(), off) {
                        (true, true) => "agent_disabled",
                        (true, false) => "agent_enabled",
                        (false, true) => "user_disabled",
                        (false, false) => "user_enabled",
                    };
                    audit(&st, actor(cu), client.ip, action, &User::key(&u.name), "");
                    saved()
                }
                Some(Err(e)) => error(account_error_text(&ctx, &e)).await,
                None => error(ctx.t("ops.failed").into()).await,
            }
        }
        "agent_add" => {
            let cfg = st.cfg.clone();
            match blocking(move || agent::create(&db, &cfg, &name, &role, now())).await {
                Some(Ok(a)) => {
                    audit(
                        &st,
                        actor(cu),
                        client.ip,
                        "agent_created",
                        &User::key(&a.name),
                        &a.role,
                    );
                    saved()
                }
                Some(Err(e)) => error(agent_error_text(&ctx, &e)).await,
                None => error(ctx.t("ops.failed").into()).await,
            }
        }
        "agent_token" => {
            let Ok(days) = form.expires.parse::<u32>() else {
                return fail(&ctx, Fail::BadRequest);
            };
            if !TOKEN_DAYS.contains(&days) {
                return fail(&ctx, Fail::BadRequest);
            }
            let t = now();
            let expires = (days > 0).then(|| t + i64::from(days) * 86_400);
            let (label, by) = (form.label.clone(), actor(cu));
            match blocking(move || agent::create_token(&db, &name, &label, expires, &by, t)).await {
                Some(Ok((token, rec))) => {
                    audit(
                        &st,
                        actor(cu),
                        client.ip,
                        "agent_token_created",
                        &rec.agent,
                        &format!("{} {}", rec.id, rec.label),
                    );
                    let note = match rec.expires {
                        Some(e) => ctx.tf("users.token_once_until", &[("time", &ctx.datetime(e))]),
                        None => ctx.t("users.token_once").to_string(),
                    };
                    secret_page(
                        &ctx,
                        &ctx.tf(
                            "users.token_title",
                            &[("label", &rec.label), ("agent", &form.name)],
                        ),
                        &[(ctx.t("users.token"), &token)],
                        &note,
                        back,
                    )
                }
                Some(Err(e)) => error(agent_error_text(&ctx, &e)).await,
                None => error(ctx.t("ops.failed").into()).await,
            }
        }
        "agent_revoke" => {
            let id = form.id.clone();
            let r = blocking(move || {
                let a = agent::get(&db, &name)?;
                Ok::<_, AgentError>(agent::revoke(&db, &User::key(&a.name), &id)?)
            })
            .await;
            match r {
                Some(Ok(Some(t))) => {
                    audit(
                        &st,
                        actor(cu),
                        client.ip,
                        "agent_token_revoked",
                        &t.agent,
                        &format!("{} {}", t.id, t.label),
                    );
                    saved()
                }
                Some(Err(e)) => error(agent_error_text(&ctx, &e)).await,
                _ => error(ctx.t("ops.failed").into()).await,
            }
        }
        _ => fail(&ctx, Fail::BadRequest),
    }
}

// ---- admin: audit log -------------------------------------------------------------------

/// GET `/.cc/admin/audit?actor=…&action=…`: the newest entries, filtered by parts of the
/// actor (`user:jana`, `agent:`, `cli`, `anon`) and of the action.
pub async fn audit_page(
    State(st): State<Shared>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    if admin_of(&principal, &current).is_none() {
        return fail(&ctx, Fail::NotFound);
    }
    let q = Query::parse(uri.query());
    let who = q.get("actor").unwrap_or("").trim().to_lowercase();
    let what = q.get("action").unwrap_or("").trim().to_lowercase();
    let st2 = st.clone();
    let events: Vec<AuditEvent> = blocking(move || st2.db.recent_audit(AUDIT_SCAN))
        .await
        .and_then(Result::ok)
        .unwrap_or_default()
        .into_iter()
        .filter(|e| who.is_empty() || e.actor.to_lowercase().contains(&who))
        .filter(|e| what.is_empty() || e.action.to_lowercase().contains(&what))
        .take(AUDIT_SHOW)
        .collect();
    let title = ctx.t("audit.title");
    layout(
        &ctx,
        title,
        html! {
            h1 { (title) }
            p { a href=(USERS) { (ctx.t("audit.users_link")) } }
            form class="audit__filter" method="get" action="/.cc/admin/audit" {
                label class="field" {
                    span class="field__label" { (ctx.t("audit.actor")) }
                    input class="field__input" type="search" name="actor" value=(q.get("actor").unwrap_or(""))
                        placeholder="user:jana, agent:, cli, anon";
                }
                label class="field" {
                    span class="field__label" { (ctx.t("audit.action")) }
                    input class="field__input" type="search" name="action" value=(q.get("action").unwrap_or(""))
                        placeholder="login, share, mcp_";
                }
                button type="submit" { (ctx.t("audit.filter")) }
            }
            @if events.is_empty() {
                p { (ctx.t("audit.none")) }
            } @else {
                p { (ctx.tn("audit.shown", events.len() as u64)) }
                table class="audit-table" {
                    thead { tr {
                        th scope="col" { (ctx.t("audit.time")) }
                        th scope="col" { (ctx.t("audit.actor")) }
                        th scope="col" { (ctx.t("audit.ip")) }
                        th scope="col" { (ctx.t("audit.action")) }
                        th scope="col" { (ctx.t("audit.target")) }
                        th scope="col" { (ctx.t("audit.detail")) }
                    } }
                    tbody {
                        @for e in &events {
                            tr {
                                td { time datetime=(e.time) { (ctx.datetime(e.time)) } }
                                td { a href=(format!("/.cc/admin/audit?actor={}", enc(&e.actor))) { (e.actor) } }
                                td { (e.ip) }
                                td { (e.action) }
                                td { (e.target) }
                                td { (e.detail) }
                            }
                        }
                    }
                }
            }
        },
    )
    .into_response()
}
