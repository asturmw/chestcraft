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
use super::views::{
    Adder, AdminPage, Ctx, SecretField, adder, confirm_dialog, form_field, layout, row_action,
    secret_box, secret_result, tag,
};
use super::{AppState, ClientInfo, Current, CurrentUser, Shared, page_ctx};
use crate::auth::accounts::{self, AccountError};
use crate::auth::agent::{self, AgentError};
use crate::auth::apitoken;
use crate::auth::apppass;
use crate::auth::perm::{ADMIN_ROLE, Principal};
use crate::db::{AgentToken, ApiToken, AppPassword, AuditEvent, Session, User};
use axum::extract::{Extension, Form, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Redirect, Response};
use maud::{Markup, html};
use serde::Deserialize;

const ACCOUNT: &str = "/.cc/account";
const USERS: &str = "/.cc/admin/users";
const API_TOKENS: &str = "/.cc/admin/api-tokens";
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

/// Secrets shown once (`secret-0`, `secret-1`, … in the shared box), with the way back.
fn secret_page(
    ctx: &Ctx,
    title: &str,
    rows: &[(&str, &str)],
    note: &str,
    back: (&str, &str),
) -> Response {
    let mut ctx = ctx.clone();
    ctx.scripts.push("copy.js");
    let ids: Vec<String> = (0..rows.len()).map(|i| format!("secret-{i}")).collect();
    let fields: Vec<SecretField> = rows
        .iter()
        .zip(&ids)
        .map(|((label, value), id)| SecretField {
            id,
            label,
            value,
            copy: ctx.t("access.copy"),
        })
        .collect();
    let body = secret_result(title, secret_box(note, &fields), back);
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

/// Last use for a table cell: "date from address", or "never".
fn used(ctx: &Ctx, at: Option<i64>, ip: Option<&str>) -> String {
    match (at, ip) {
        (Some(t), Some(ip)) => ctx.tf(
            "api_tokens.used_from",
            &[("date", &ctx.datetime(t)), ("ip", ip)],
        ),
        (Some(t), None) => ctx.datetime(t),
        _ => ctx.t("api_tokens.never_used").to_string(),
    }
}

/// The app password and session sections of the account page. `q` carries the outcome of
/// the last form (`app=revoked|<error>`, `signed_out=<n>`): a refused new password opens
/// its form again with the reason.
pub fn access_sections(ctx: &Ctx, csrf: &str, v: &AccessView, q: &Query) -> Markup {
    let code = q.get("app");
    let add_error = code.and_then(|code| match code {
        "label" => Some(ctx.tf(
            "access.bad_label",
            &[("max", &apppass::MAX_LABEL_CHARS.to_string())],
        )),
        "duplicate" => Some(ctx.t("access.app_duplicate").to_string()),
        "too_many" => Some(ctx.tf(
            "access.app_too_many",
            &[("max", &apppass::MAX_PER_USER.to_string())],
        )),
        _ => None,
    });
    let app_msg = code.filter(|_| add_error.is_none()).map(|code| match code {
        "revoked" => ("ok", ctx.t("access.app_revoked")),
        _ => ("error", ctx.t("ops.failed")),
    });
    let others = v.sessions.iter().filter(|(me, _)| !me).count() as u64;
    let action = "/.cc/account/app-passwords";
    html! {
        section class="admin-section account-card account-card--wide account__app-passwords" id="app-passwords" aria-labelledby="app-passwords-title" {
            div class="admin-section__head" {
                h2 class="account-card__title" id="app-passwords-title" { (ctx.t("access.app_passwords")) }
                (adder(ctx, &Adder {
                    id: "add-app-password",
                    button: ctx.t("access.app_password"),
                    plus: true,
                    label: ctx.t("access.app_add"),
                    title: ctx.t("access.app_add"),
                    action,
                    submit: ctx.t("access.app_add"),
                    error: add_error.as_deref(),
                }, html! {
                    input type="hidden" name="csrf" value=(csrf);
                    input type="hidden" name="op" value="add";
                    (form_field("app-label", ctx.t("access.device"), html! {
                        div class="field__control" {
                            input class="field__input" id="app-label" type="text" name="label" required
                                maxlength=(apppass::MAX_LABEL_CHARS) placeholder=(ctx.t("access.device_hint")) autocomplete="off";
                        }
                    }, None))
                }))
            }
            p class="admin-section__intro" { (ctx.t("access.app_intro")) }
            @if let Some((state, text)) = app_msg {
                p class="banner" data-state=(state) role="status" data-js=[(state == "error").then_some("form-error")] { (text) }
            }
            @if v.app_passwords.is_empty() {
                p class="admin-empty" { (ctx.t("access.app_none")) }
            } @else {
                table class="admin-table" {
                    thead { tr {
                        th scope="col" { (ctx.t("access.device")) }
                        th scope="col" { (ctx.t("api_tokens.created")) }
                        th scope="col" { (ctx.t("api_tokens.last_used")) }
                        th scope="col" { span class="vh" { (ctx.t("api_tokens.actions")) } }
                    } }
                    tbody {
                        @for r in &v.app_passwords {
                            tr {
                                th scope="row" { (r.label) }
                                td data-label=(ctx.t("api_tokens.created")) { (ctx.datetime(r.created)) }
                                td data-label=(ctx.t("api_tokens.last_used")) { (used(ctx, r.last_used, r.last_ip.as_deref())) }
                                td class="actions-cell" { div class="row-actions" {
                                    (row_action(action, html! {
                                        input type="hidden" name="csrf" value=(csrf);
                                        input type="hidden" name="op" value="revoke";
                                        input type="hidden" name="id" value=(r.id);
                                    }, ctx.t("access.revoke"), Some(&ctx.tf("access.revoke_confirm", &[("label", &r.label)]))))
                                } }
                            }
                        }
                    }
                }
            }
        }
        section class="account-card account__sessions" id="sessions" aria-labelledby="sessions-title" {
            h2 class="account-card__title" id="sessions-title" { (ctx.t("access.sessions")) }
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
                form method="post" action="/.cc/account/sessions"
                    data-confirm=(ctx.tn("access.sign_out_others_confirm", others)) {
                    input type="hidden" name="csrf" value=(csrf);
                    button class="btn btn--danger" type="submit" { (ctx.tn("access.sign_out_others", others)) }
                }
            }
        }
        (confirm_dialog(ctx))
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
    /// Status API tokens (review Q27: with the agents under "Machines").
    pub api_tokens: Vec<ApiToken>,
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
        let mut api_tokens: Vec<ApiToken> = st
            .db
            .api_tokens()
            .unwrap_or_default()
            .into_iter()
            .map(|(_, t)| t)
            .collect();
        api_tokens.sort_by_key(|t| t.created);
        let mut roles = vec![ADMIN_ROLE.to_string()];
        roles.extend(st.cfg.roles.keys().cloned());
        UsersData {
            people,
            agents,
            api_tokens,
            roles,
            mcp_on: st.cfg.mcp.listen.is_some(),
        }
    })
    .await
    .unwrap_or(UsersData {
        people: Vec::new(),
        agents: Vec::new(),
        api_tokens: Vec::new(),
        roles: Vec::new(),
        mcp_on: false,
    })
}

fn role_select(id: &str, roles: &[String], current: &str, agents: bool) -> Markup {
    html! {
        select class="field__input" id=(id) name="role" {
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

/// The outcome shown on the accounts page: a banner on top, or (`adder`) inside the create
/// form that failed, which opens again.
pub struct Msg {
    state: &'static str,
    text: String,
    adder: Option<String>,
}

impl Msg {
    fn ok(text: String) -> Msg {
        Msg {
            state: "ok",
            text,
            adder: None,
        }
    }

    fn error(text: String, adder: Option<String>) -> Msg {
        Msg {
            state: "error",
            text,
            adder,
        }
    }

    /// The error for the create form `id`, if it failed.
    fn for_adder(msg: Option<&Msg>, id: &str) -> Option<String> {
        msg.filter(|m| m.adder.as_deref() == Some(id))
            .map(|m| m.text.clone())
    }
}

/// The role of a row: a select with its button (a role change is not undone by a slip of
/// the select, so it doesn't post on change); one's own role as text.
fn role_cell(ctx: &Ctx, csrf: &str, u: &User, roles: &[String], me: bool) -> Markup {
    let id = format!("role-{}", u.name);
    html! {
        @if me {
            (u.role)
        } @else {
            form class="inline-form" method="post" action=(USERS) {
                (hidden(csrf, "role", &u.name))
                label class="vh" for=(id) { (ctx.tf("users.role_of", &[("name", &u.name)])) }
                (role_select(&id, roles, &u.role, u.is_agent()))
                button class="btn btn--quiet" type="submit" { (ctx.t("users.set_role")) }
            }
        }
    }
}

/// Block (asks first) or unblock.
fn block_action(ctx: &Ctx, csrf: &str, u: &User) -> Markup {
    if u.disabled {
        row_action(
            USERS,
            hidden(csrf, "enable", &u.name),
            ctx.t("users.enable"),
            None,
        )
    } else {
        row_action(
            USERS,
            hidden(csrf, "disable", &u.name),
            ctx.t("users.disable"),
            Some(&ctx.tf("users.disable_confirm", &[("name", &u.name)])),
        )
    }
}

fn state_tags(ctx: &Ctx, st: &AppState, u: &User) -> Markup {
    html! {
        @if u.disabled { " " (tag(ctx.t("users.blocked"), None, Some("off"))) }
        @if u.must_change_password { " " (tag(ctx.t("users.must_change"), None, None)) }
        @if !accounts::role_exists(&st.cfg, &u.role) { " " (tag(ctx.t("users.role_missing"), None, Some("warning"))) }
    }
}

fn text_field(id: &str, label: &str, name: &str, max: usize, placeholder: Option<&str>) -> Markup {
    form_field(
        id,
        label,
        html! {
            div class="field__control" {
                input class="field__input" id=(id) type="text" name=(name) required maxlength=(max)
                    autocomplete="off" spellcheck="false" placeholder=[placeholder];
            }
        },
        None,
    )
}

fn select_field(id: &str, label: &str, select: Markup) -> Markup {
    form_field(
        id,
        label,
        html! { div class="field__control" { (select) } },
        None,
    )
}

/// The accounts page (review Q26, Q27): people, then "Machines" (agents with their MCP
/// tokens, status API tokens). Each part leads with its table; the create forms sit behind
/// "+ …" buttons, the row actions in the rows.
fn users_view(
    ctx: &Ctx,
    st: &AppState,
    cu: &CurrentUser,
    d: &UsersData,
    msg: Option<Msg>,
) -> Markup {
    let title = ctx.t("users.title");
    let csrf = cu.csrf.as_str();
    let msg = msg.as_ref();
    let err = |id: &str| Msg::for_adder(msg, id);
    let mut ctx = ctx.clone();
    ctx.scripts.extend(["copy.js", "forms.js"]);
    let ctx = &ctx;
    let default_role = d.roles.get(1).map(String::as_str).unwrap_or(ADMIN_ROLE);
    layout(
        ctx,
        title,
        html! {
            h1 class="users__title" { (title) }
            @if let Some(m) = msg.filter(|m| m.adder.is_none()) {
                p class="banner" data-state=(m.state) role="status"
                    data-js=[(m.state == "error").then_some("form-error")] { (m.text) }
            }
            section class="admin-section" id="people" aria-labelledby="people-title" {
                div class="admin-section__head" {
                    h2 id="people-title" { (ctx.t("users.people")) }
                    (adder(ctx, &Adder {
                        id: "add-person",
                        button: ctx.t("nav.account"),
                        plus: true,
                        label: ctx.t("users.add"),
                        title: ctx.t("users.add_title"),
                        action: USERS,
                        submit: ctx.t("users.add"),
                        error: err("add-person").as_deref(),
                    }, html! {
                        input type="hidden" name="csrf" value=(csrf);
                        input type="hidden" name="op" value="add";
                        (text_field("person-name", ctx.t("users.name"), "name", 32, None))
                        (select_field("person-role", ctx.t("users.role"), role_select("person-role", &d.roles, default_role, false)))
                    }))
                }
                table class="admin-table users-table" {
                    thead { tr {
                        th scope="col" { (ctx.t("users.name")) }
                        th scope="col" { (ctx.t("users.role")) }
                        th scope="col" { (ctx.t("users.last_login")) }
                        th scope="col" { span class="vh" { (ctx.t("users.actions")) } }
                    } }
                    tbody {
                        @for u in &d.people {
                            @let me = User::key(&u.name) == cu.key;
                            tr data-state=(if u.disabled { "disabled" } else { "active" }) {
                                th scope="row" { span class="admin-table__name" { (u.name) } (state_tags(ctx, st, u)) }
                                td data-label=(ctx.t("users.role")) { (role_cell(ctx, csrf, u, &d.roles, me)) }
                                td data-label=(ctx.t("users.last_login")) {
                                    @match u.last_login { Some(t) => (ctx.datetime(t)), None => (ctx.t("users.never")) }
                                }
                                td class="actions-cell" { div class="row-actions" {
                                    (row_action(USERS, hidden(csrf, "passwd", &u.name), ctx.t("users.passwd"),
                                        Some(&ctx.tf("users.passwd_confirm", &[("name", &u.name)]))))
                                    @if !me { (block_action(ctx, csrf, u)) }
                                } }
                            }
                        }
                    }
                }
            }
            section class="admin-section" id="machines" aria-labelledby="machines-title" {
                h2 id="machines-title" { (ctx.t("users.machines")) }
                div class="admin-subsection" id="agents" {
                    div class="admin-section__head" {
                        h3 id="agents-title" { (ctx.t("users.agents")) }
                        (adder(ctx, &Adder {
                            id: "add-agent",
                            button: ctx.t("users.agent_badge"),
                            plus: true,
                            label: ctx.t("users.agent_add"),
                            title: ctx.t("users.agent_add_title"),
                            action: USERS,
                            submit: ctx.t("users.agent_add"),
                            error: err("add-agent").as_deref(),
                        }, html! {
                            input type="hidden" name="csrf" value=(csrf);
                            input type="hidden" name="op" value="agent_add";
                            (text_field("agent-name", ctx.t("users.name"), "name", 32, None))
                            (select_field("agent-role", ctx.t("users.role"), role_select("agent-role", &d.roles, "", true)))
                        }))
                    }
                    p class="admin-section__intro" { (ctx.t("users.agents_intro")) }
                    @if !d.mcp_on { p class="banner" data-state="warning" { (ctx.t("users.mcp_off")) } }
                    @if d.agents.is_empty() {
                        p class="admin-empty" { (ctx.t("users.agents_none")) }
                    } @else {
                        table class="admin-table agents-table" {
                            thead { tr {
                                th scope="col" { (ctx.t("users.name")) }
                                th scope="col" { (ctx.t("users.role")) }
                                th scope="col" { (ctx.t("users.tokens")) }
                                th scope="col" { span class="vh" { (ctx.t("users.actions")) } }
                            } }
                            tbody {
                                @for (a, tokens) in &d.agents {
                                    @let adder_id = format!("add-token-{}", a.name);
                                    tr data-state=(if a.disabled { "disabled" } else { "active" }) {
                                        th scope="row" { span class="admin-table__name" { (a.name) } (state_tags(ctx, st, a)) }
                                        td data-label=(ctx.t("users.role")) { (role_cell(ctx, csrf, a, &d.roles, false)) }
                                        td class="tokens-cell" data-label=(ctx.t("users.tokens")) {
                                            @if tokens.is_empty() {
                                                p class="admin-empty" { (ctx.t("users.tokens_none")) }
                                            } @else {
                                                ul class="token-list" {
                                                    @for t in tokens {
                                                        li class="token-list__item" data-state=(if t.expires.is_some_and(|e| e <= now()) { "expired" } else { "active" }) {
                                                            span class="token-list__text" {
                                                                strong { (t.label) }
                                                                span class="token-list__meta" {
                                                                    @if let Some(e) = t.expires {
                                                                        (ctx.tf("users.expires_at", &[("time", &ctx.datetime(e))])) " · "
                                                                    }
                                                                    (ctx.t("api_tokens.last_used")) ": " (used(ctx, t.last_used, t.last_ip.as_deref()))
                                                                }
                                                            }
                                                            (row_action(USERS, html! {
                                                                (hidden(csrf, "agent_revoke", &a.name))
                                                                input type="hidden" name="id" value=(t.id);
                                                            }, ctx.t("access.revoke"), Some(&ctx.tf("access.revoke_confirm", &[("label", &t.label)]))))
                                                        }
                                                    }
                                                }
                                            }
                                            @let token_title = ctx.tf("users.token_add_title", &[("agent", &a.name)]);
                                            (adder(ctx, &Adder {
                                                id: &adder_id,
                                                button: ctx.t("users.token"),
                                                plus: true,
                                                label: &token_title,
                                                title: &token_title,
                                                action: USERS,
                                                submit: ctx.t("users.token_add"),
                                                error: err(&adder_id).as_deref(),
                                            }, html! {
                                                (hidden(csrf, "agent_token", &a.name))
                                                (text_field(&format!("{adder_id}-label"), ctx.t("users.token_host"), "label", agent::MAX_LABEL_CHARS, None))
                                                (select_field(&format!("{adder_id}-expires"), ctx.t("users.token_expires"), html! {
                                                    select class="field__input" id=(format!("{adder_id}-expires")) name="expires" {
                                                        @for days in TOKEN_DAYS {
                                                            option value=(days) selected[days == 90] {
                                                                @if days == 0 { (ctx.t("users.expires_never")) }
                                                                @else { (ctx.tn("users.expires_days", u64::from(days))) }
                                                            }
                                                        }
                                                    }
                                                }))
                                            }))
                                        }
                                        td class="actions-cell" { div class="row-actions" { (block_action(ctx, csrf, a)) } }
                                    }
                                }
                            }
                        }
                    }
                }
                div class="admin-subsection" id="api-tokens" {
                    div class="admin-section__head" {
                        h3 id="api-tokens-title" { (ctx.t("api_tokens.title")) }
                        (adder(ctx, &Adder {
                            id: "add-api-token",
                            button: ctx.t("users.token"),
                            plus: true,
                            label: ctx.t("api_tokens.add"),
                            title: ctx.t("api_tokens.add_title"),
                            action: API_TOKENS,
                            submit: ctx.t("api_tokens.add"),
                            error: err("add-api-token").as_deref(),
                        }, html! {
                            input type="hidden" name="csrf" value=(csrf);
                            input type="hidden" name="op" value="add";
                            (text_field("api-token-label", ctx.t("api_tokens.label"), "label", apitoken::MAX_LABEL_CHARS, Some(ctx.t("api_tokens.label_example"))))
                        }))
                    }
                    p class="admin-section__intro" { (ctx.t("api_tokens.intro")) }
                    @if d.api_tokens.is_empty() {
                        p class="admin-empty" { (ctx.t("api_tokens.none")) }
                    } @else {
                        table class="admin-table api-tokens-table" {
                            thead { tr {
                                th scope="col" { (ctx.t("api_tokens.label")) }
                                th scope="col" { (ctx.t("api_tokens.created")) }
                                th scope="col" { (ctx.t("api_tokens.last_used")) }
                                th scope="col" { span class="vh" { (ctx.t("api_tokens.actions")) } }
                            } }
                            tbody {
                                @for t in &d.api_tokens {
                                    tr data-id=(t.id) {
                                        th scope="row" { (t.label) }
                                        td data-label=(ctx.t("api_tokens.created")) { (ctx.datetime(t.created)) }
                                        td data-label=(ctx.t("api_tokens.last_used")) { (used(ctx, t.last_used, t.last_ip.as_deref())) }
                                        td class="actions-cell" { div class="row-actions" {
                                            (row_action(API_TOKENS, html! {
                                                input type="hidden" name="csrf" value=(csrf);
                                                input type="hidden" name="op" value="revoke";
                                                input type="hidden" name="id" value=(t.id);
                                            }, ctx.t("api_tokens.revoke"), Some(&ctx.tf("access.revoke_confirm", &[("label", &t.label)]))))
                                        } }
                                    }
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
    let ctx = ctx.admin_page(AdminPage::Users);
    let q = Query::parse(uri.query());
    let msg = q.has("saved").then(|| Msg::ok(ctx.t("users.saved").into()));
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
    let ctx = ctx.admin_page(AdminPage::Users);
    if !csrf_ok(&headers, &st, cu, &form.csrf) {
        return forbidden(&ctx);
    }
    // A refused create opens its form again with the reason.
    let failed_form = match form.op.as_str() {
        "add" => Some("add-person".to_string()),
        "agent_add" => Some("add-agent".to_string()),
        "agent_token" => Some(format!("add-token-{}", form.name.trim())),
        _ => None,
    };
    let error = |text: String| {
        let (st, ctx, cu) = (st.clone(), ctx.clone(), cu.clone());
        let msg = Msg::error(text, failed_form.clone());
        async move {
            let d = users_data(&st).await;
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                users_view(&ctx, &st, &cu, &d, Some(msg)),
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
                        (&format!("{USERS}#agents"), ctx.t("users.back")),
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

// ---- admin: status API tokens (with the agents, review Q27) -------------------------------

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct TokenForm {
    csrf: String,
    op: String,
    label: String,
    id: String,
}

/// POST `/.cc/admin/api-tokens`: create (shown once) or revoke a status API token; the
/// answers go back to the accounts page.
pub async fn tokens_submit(
    State(st): State<Shared>,
    Extension(client): Extension<ClientInfo>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
    Form(form): Form<TokenForm>,
) -> Response {
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    let Some(cu) = admin_of(&principal, &current) else {
        return fail(&ctx, Fail::NotFound);
    };
    let ctx = ctx.admin_page(AdminPage::Users);
    if !csrf_ok(&headers, &st, cu, &form.csrf) {
        return forbidden(&ctx);
    }
    let rerender = |text: String, adder: Option<&str>| {
        let (st, ctx, cu) = (st.clone(), ctx.clone(), cu.clone());
        let msg = Msg::error(text, adder.map(str::to_string));
        async move {
            let d = users_data(&st).await;
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                users_view(&ctx, &st, &cu, &d, Some(msg)),
            )
                .into_response()
        }
    };
    match form.op.as_str() {
        "add" => {
            let created = {
                let (st, label, by) = (st.clone(), form.label.clone(), actor(cu));
                blocking(move || apitoken::create(&st.db, &label, &by, now())).await
            };
            let (token, rec) = match created {
                Some(Ok(t)) => t,
                Some(Err(e)) => {
                    let msg = match e {
                        apitoken::CreateError::BadLabel => ctx.tf(
                            "api_tokens.bad_label",
                            &[("max", &apitoken::MAX_LABEL_CHARS.to_string())],
                        ),
                        apitoken::CreateError::Duplicate => {
                            ctx.t("api_tokens.duplicate").to_string()
                        }
                        apitoken::CreateError::TooMany => ctx.tf(
                            "api_tokens.too_many",
                            &[("max", &apitoken::MAX_TOKENS.to_string())],
                        ),
                        apitoken::CreateError::Db(_) => ctx.t("ops.failed").to_string(),
                    };
                    return rerender(msg, Some("add-api-token")).await;
                }
                None => return rerender(ctx.t("ops.failed").into(), Some("add-api-token")).await,
            };
            audit(
                &st,
                actor(cu),
                client.ip,
                "api_token_created",
                &rec.label,
                &rec.id,
            );
            api_token_created(&ctx, &rec.label, &token)
        }
        "revoke" => {
            let revoked = {
                let (st, id) = (st.clone(), form.id.clone());
                blocking(move || apitoken::revoke(&st.db, &id)).await
            };
            match revoked {
                Some(Ok(Some(rec))) => {
                    audit(
                        &st,
                        actor(cu),
                        client.ip,
                        "api_token_revoked",
                        &rec.label,
                        &rec.id,
                    );
                    Redirect::to(&format!("{USERS}#api-tokens")).into_response()
                }
                Some(Ok(None)) => rerender(ctx.t("api_tokens.gone").into(), None).await,
                _ => rerender(ctx.t("ops.failed").into(), None).await,
            }
        }
        _ => rerender(ctx.t("ops.invalid").into(), None).await,
    }
}

/// A new status API token, shown once, with an example request.
fn api_token_created(ctx: &Ctx, label: &str, token: &str) -> Response {
    let mut ctx = ctx.clone();
    ctx.scripts.push("copy.js");
    let title = ctx.tf("api_tokens.created_title", &[("label", label)]);
    let body = html! {
        (secret_box(ctx.t("api_tokens.once"), &[SecretField {
            id: "secret-token",
            label: ctx.t("api_tokens.token"),
            value: token,
            copy: ctx.t("api_tokens.copy"),
        }]))
        p { (ctx.t("api_tokens.usage")) }
        pre class="secret-result__example" { code { "curl -H 'Authorization: Bearer " (token) "' http://<host>/api/v1/status" } }
    };
    let back = format!("{USERS}#api-tokens");
    no_store(
        layout(
            &ctx,
            &title,
            secret_result(&title, body, (&back, ctx.t("users.back"))),
        )
        .into_response(),
    )
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
    let ctx = ctx.admin_page(AdminPage::Audit);
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
