//! Sign-in, sign-out, password change, and the session behind each request.

use super::views::{self, Ctx, LoginView, PasswordView};
use super::{AppState, ClientInfo, Shared, cookie, page_ctx};
use crate::auth::perm::{ADMIN_ROLE, Origin, Principal};
use crate::auth::{apppass, password, session};
use crate::dav::{DavAuth, DavUser};
use crate::db::{AuditEvent, Session, User, UserSettings};
use axum::extract::{Extension, Form, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use std::net::IpAddr;
use std::sync::Arc;

/// The signed-in account behind a request.
#[derive(Debug, Clone)]
pub struct CurrentUser {
    pub key: String,
    pub name: String,
    pub role: String,
    pub settings: UserSettings,
    pub csrf: String,
    pub session_hash: String,
    pub must_change_password: bool,
}

/// Request extension: the signed-in account, if any.
pub type Current = Option<Arc<CurrentUser>>;

pub fn now() -> i64 {
    jiff::Timestamp::now().as_second()
}

/// What a user who must change the password can still reach.
fn allowed_before_password_change(path: &str) -> bool {
    matches!(
        path,
        "/.cc/password" | "/.cc/logout" | "/healthz" | "/robots.txt" | "/favicon.ico"
    ) || path.starts_with("/.cc/assets/")
}

/// Resolve the session cookie into a principal and the current account.
pub async fn principal_layer(
    State(st): State<Shared>,
    Extension(client): Extension<ClientInfo>,
    mut req: Request,
    next: Next,
) -> Response {
    #[cfg(test)]
    let test_override = st.test_principal.lock().unwrap().clone();
    #[cfg(test)]
    if let Some(p) = test_override {
        req.extensions_mut().insert(p);
        req.extensions_mut().insert(None::<Arc<CurrentUser>>);
        req.extensions_mut().insert(None::<Arc<DavUser>>);
        return next.run(req).await;
    }
    // WebDAV clients send Basic auth with an app password. From the internet the header is
    // ignored (WebDAV refuses those requests anyway, and pages fall back to the session).
    if client.origin == Origin::Local
        && let Some((user, secret)) = basic_credentials(req.headers())
    {
        return match check_basic(&st, &client, &user, &secret).await {
            Ok(dav) => {
                req.extensions_mut()
                    .insert(Principal::from_account(&dav.0.name, &dav.0.role));
                req.extensions_mut().insert(None::<Arc<CurrentUser>>);
                req.extensions_mut().insert(Some(dav.1) as DavAuth);
                next.run(req).await
            }
            Err(res) => *res,
        };
    }
    req.extensions_mut().insert(None::<Arc<DavUser>>);
    let resolved = match cookie(req.headers(), session::COOKIE) {
        Some(v) if session::plausible(v) => resolve(&st, client.origin, v).await,
        _ => None,
    };
    let (principal, current) = match resolved {
        Some(cu) => (
            Principal::from_account(&cu.name, &cu.role),
            Some(Arc::new(cu)),
        ),
        None => (Principal::Anonymous, None),
    };
    if current.as_ref().is_some_and(|c| c.must_change_password)
        && !allowed_before_password_change(req.uri().path())
    {
        return Redirect::to("/.cc/password").into_response();
    }
    req.extensions_mut().insert(principal);
    req.extensions_mut().insert(current);
    next.run(req).await
}

/// User name and password from `Authorization: Basic …`.
pub fn basic_credentials(headers: &HeaderMap) -> Option<(String, String)> {
    let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?.trim();
    let (scheme, data) = v.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }
    let decoded = String::from_utf8(base64_decode(data.trim())?).ok()?;
    let (user, pass) = decoded.split_once(':')?;
    Some((user.to_string(), pass.to_string()))
}

/// Standard base64 with padding (what Basic auth uses).
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let b = s.as_bytes();
    if !b.len().is_multiple_of(4) || b.len() > 4096 {
        return None;
    }
    let mut out = Vec::with_capacity(b.len() / 4 * 3);
    for (i, chunk) in b.chunks(4).enumerate() {
        let last = i == b.len() / 4 - 1;
        let pad = chunk.iter().rev().take_while(|&&c| c == b'=').count();
        if pad > 2 || (pad > 0 && !last) {
            return None;
        }
        let mut n = 0u32;
        for &c in &chunk[..4 - pad] {
            n = (n << 6) | val(c)?;
        }
        n <<= 6 * pad as u32;
        let bytes = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        out.extend_from_slice(&bytes[..3 - pad]);
    }
    Some(out)
}

/// Verify a WebDAV sign-in, with the same backoff as the sign-in form.
async fn check_basic(
    st: &Shared,
    client: &ClientInfo,
    user: &str,
    secret: &str,
) -> Result<(User, Arc<DavUser>), Box<Response>> {
    let account = User::key(user.trim());
    let now = now();
    let wait = st.backoff.wait(client.ip, &account, now);
    if wait > 0 {
        let mut res = (
            StatusCode::TOO_MANY_REQUESTS,
            "Too many wrong passwords; wait a moment.\n",
        )
            .into_response();
        if let Ok(v) = HeaderValue::from_str(&wait.to_string()) {
            res.headers_mut().insert(header::RETRY_AFTER, v);
        }
        return Err(Box::new(res));
    }
    let verified = {
        let (st, user, secret, ip) = (
            st.clone(),
            user.to_string(),
            secret.to_string(),
            client.ip.to_string(),
        );
        tokio::task::spawn_blocking(move || {
            let v = apppass::verify(&st.db, &user, &secret).ok().flatten()?;
            apppass::touch(&st.db, &v, &ip, now);
            Some(v)
        })
        .await
        .ok()
        .flatten()
    };
    let Some(v) = verified else {
        let f = st.backoff.fail(client.ip, &account, true, now);
        audit(
            st,
            "anon".into(),
            client.ip,
            "webdav_login_failed",
            &account,
            "",
        );
        if f.stuffing {
            tracing::warn!(account, "many failed WebDAV sign-ins from many addresses");
            audit(
                st,
                "anon".into(),
                client.ip,
                "credential_stuffing",
                &account,
                "webdav",
            );
        }
        return Err(Box::new(crate::dav::unauthorized()));
    };
    st.backoff.succeed(client.ip, &account);
    let dav = Arc::new(DavUser {
        key: v.pass.user.clone(),
        name: v.user.name.clone(),
        label: v.pass.label.clone(),
    });
    Ok((v.user, dav))
}

async fn resolve(st: &Shared, origin: Origin, cookie_value: &str) -> Option<CurrentUser> {
    let id_hash = session::hash_id(cookie_value);
    let st = st.clone();
    tokio::task::spawn_blocking(move || {
        let s = st.db.session(&id_hash).ok()??;
        let user = st.db.user(&s.user).ok()??;
        let now = now();
        let admin = user.role == ADMIN_ROLE;
        if user.disabled || !session::is_valid(&s, now, &st.cfg.sessions, admin, origin) {
            let _ = st.db.delete_session(&id_hash);
            return None;
        }
        if now - s.last_seen > session::TOUCH_EVERY {
            let touched = Session {
                last_seen: now,
                ..s.clone()
            };
            let _ = st.db.put_session(&id_hash, &touched, false);
        }
        Some(CurrentUser {
            key: s.user,
            name: user.name,
            role: user.role,
            settings: user.settings,
            csrf: s.csrf,
            session_hash: id_hash,
            must_change_password: user.must_change_password,
        })
    })
    .await
    .ok()
    .flatten()
}

/// Browsers send `Origin` with every POST; it must be this site (public URL, or the host the
/// request was sent to). Requests without `Origin` still need the CSRF token.
pub fn same_origin(headers: &HeaderMap, public_url: &str) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    if origin.eq_ignore_ascii_case(public_url) {
        return true;
    }
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
    match (origin.split_once("://"), host) {
        (Some((_, origin_host)), Some(host)) => origin_host.eq_ignore_ascii_case(host),
        _ => false,
    }
}

fn token_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// Origin and CSRF token check for state-changing requests of signed-in users.
pub fn csrf_ok(
    headers: &HeaderMap,
    st: &AppState,
    current: &CurrentUser,
    form_token: &str,
) -> bool {
    let token = headers
        .get("x-csrf-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or(form_token);
    same_origin(headers, &st.cfg.server.public_url) && token_eq(token, &current.csrf)
}

pub fn forbidden(ctx: &Ctx) -> Response {
    (
        StatusCode::FORBIDDEN,
        views::error_page(
            ctx,
            ctx.t("error.forbidden_title"),
            ctx.t("error.forbidden_body"),
        ),
    )
        .into_response()
}

/// Only local paths, so a sign-in link can't send people to another site.
pub fn safe_next(next: &str) -> String {
    let ok = next.starts_with('/')
        && !next.starts_with("//")
        && !next.starts_with("/\\")
        && !next.chars().any(char::is_control);
    if ok {
        next.to_string()
    } else {
        "/".to_string()
    }
}

/// Write an audit event without holding up the request.
pub fn audit(st: &AppState, actor: String, ip: IpAddr, action: &str, target: &str, detail: &str) {
    let ev = AuditEvent {
        time: now(),
        actor,
        ip: ip.to_string(),
        action: action.to_string(),
        target: target.to_string(),
        detail: detail.to_string(),
    };
    let db = st.db.clone();
    tokio::task::spawn_blocking(move || {
        if let Err(e) = db.audit(&ev) {
            tracing::warn!("audit write failed: {e}");
        }
    });
}

fn slowed_down(ctx: &Ctx, wait_secs: i64) -> String {
    let minutes = ((wait_secs + 59) / 60).max(1);
    ctx.tf("login.slowed_down", &[("minutes", &minutes.to_string())])
}

#[derive(Deserialize, Default)]
pub struct NextQuery {
    #[serde(default)]
    next: String,
}

pub async fn login_page(
    State(st): State<Shared>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
    Query(q): Query<NextQuery>,
) -> Response {
    if !principal.is_anonymous() {
        return Redirect::to(&safe_next(&q.next)).into_response();
    }
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    views::login(
        &ctx,
        &LoginView {
            next: safe_next(&q.next),
            username: String::new(),
            error: None,
        },
    )
    .into_response()
}

#[derive(Deserialize)]
pub struct LoginForm {
    username: String,
    password: String,
    #[serde(default)]
    next: String,
}

pub async fn login_submit(
    State(st): State<Shared>,
    Extension(client): Extension<ClientInfo>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
    Form(form): Form<LoginForm>,
) -> Response {
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    if !same_origin(&headers, &st.cfg.server.public_url) {
        return forbidden(&ctx);
    }
    let next = safe_next(&form.next);
    let account = User::key(form.username.trim());
    let now = now();
    let show_error = |msg: String, status: StatusCode| -> Response {
        let view = LoginView {
            next: next.clone(),
            username: form.username.clone(),
            error: Some(msg),
        };
        (status, views::login(&ctx, &view)).into_response()
    };

    let wait = st.backoff.wait(client.ip, &account, now);
    if wait > 0 {
        return show_error(slowed_down(&ctx, wait), StatusCode::TOO_MANY_REQUESTS);
    }

    let checked = {
        let (st, account, pw) = (st.clone(), account.clone(), form.password.clone());
        tokio::task::spawn_blocking(move || match st.db.user(&account).ok().flatten() {
            Some(u) => (password::verify(&pw, &u.password_hash) && !u.disabled).then_some(u),
            None => {
                password::verify_dummy(&pw);
                None
            }
        })
        .await
        .ok()
        .flatten()
    };

    let Some(user) = checked else {
        let f = st
            .backoff
            .fail(client.ip, &account, client.origin == Origin::Local, now);
        audit(&st, "anon".into(), client.ip, "login_failed", &account, "");
        if f.stuffing {
            tracing::warn!(account, "many failed sign-ins from many addresses");
            audit(
                &st,
                "anon".into(),
                client.ip,
                "credential_stuffing",
                &account,
                "",
            );
        }
        let msg = if f.wait > 0 {
            slowed_down(&ctx, f.wait)
        } else {
            ctx.t("login.wrong").to_string()
        };
        return show_error(msg, StatusCode::UNAUTHORIZED);
    };

    st.backoff.succeed(client.ip, &account);
    let admin = user.role == ADMIN_ROLE;
    let must_change = user.must_change_password;
    let remote = client.origin == Origin::Remote;
    let created = session::create(&account, client.ip, remote, now);
    let stored = {
        let (st, account, hash, sess, ip) = (
            st.clone(),
            account.clone(),
            created.id_hash.clone(),
            created.session.clone(),
            client.ip.to_string(),
        );
        tokio::task::spawn_blocking(move || -> crate::db::Result<bool> {
            st.db.put_session(&hash, &sess, true)?;
            let mut u = st.db.user(&account)?.unwrap_or(user);
            u.last_login = Some(now);
            let new_ip = remote && !u.known_remote_ips.contains(&ip);
            if new_ip {
                u.known_remote_ips.push(ip);
                let excess = u.known_remote_ips.len().saturating_sub(20);
                u.known_remote_ips.drain(..excess);
            }
            st.db.put_user(&u)?;
            Ok(new_ip)
        })
        .await
    };
    let new_remote_ip = match stored {
        Ok(Ok(b)) => b,
        _ => {
            tracing::error!("can't store the session");
            return show_error(
                ctx.t("error.server_error_body").to_string(),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };
    let actor = format!("user:{account}");
    audit(
        &st,
        actor.clone(),
        client.ip,
        "login",
        &account,
        if remote { "remote" } else { "local" },
    );
    if admin && new_remote_ip {
        tracing::warn!(account, ip = %client.ip, "admin signed in from a new remote address");
        audit(&st, actor, client.ip, "admin_new_remote_ip", &account, "");
    }

    let target = if must_change {
        "/.cc/password".to_string()
    } else {
        next.clone()
    };
    let cookie = session::set_cookie(
        &created.cookie_value,
        session::max_age(&st.cfg.sessions, admin, client.origin),
        client.https,
    );
    let mut res = Redirect::to(&target).into_response();
    if let Ok(v) = HeaderValue::from_str(&cookie) {
        res.headers_mut().insert(header::SET_COOKIE, v);
    }
    res
}

#[derive(Deserialize, Default)]
pub struct CsrfForm {
    #[serde(default)]
    csrf: String,
}

pub async fn logout(
    State(st): State<Shared>,
    Extension(client): Extension<ClientInfo>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
    Form(form): Form<CsrfForm>,
) -> Response {
    let Some(cu) = current.clone() else {
        return Redirect::to("/").into_response();
    };
    if !csrf_ok(&headers, &st, &cu, &form.csrf) {
        let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
        return forbidden(&ctx);
    }
    {
        let (st, hash) = (st.clone(), cu.session_hash.clone());
        let _ = tokio::task::spawn_blocking(move || st.db.delete_session(&hash)).await;
    }
    audit(
        &st,
        format!("user:{}", cu.key),
        client.ip,
        "logout",
        &cu.key,
        "",
    );
    let mut res = Redirect::to("/").into_response();
    if let Ok(v) = HeaderValue::from_str(&session::set_cookie("", 0, client.https)) {
        res.headers_mut().insert(header::SET_COOKIE, v);
    }
    res
}

pub async fn password_page(
    State(st): State<Shared>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
) -> Response {
    let Some(cu) = current.clone() else {
        return Redirect::to("/.cc/login?next=%2F.cc%2Fpassword").into_response();
    };
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    let view = PasswordView {
        forced: cu.must_change_password,
        csrf: cu.csrf.clone(),
        error: None,
        saved: false,
    };
    views::password(&ctx, &view).into_response()
}

#[derive(Deserialize)]
pub struct PasswordForm {
    #[serde(default)]
    current: String,
    new: String,
    repeat: String,
    #[serde(default)]
    csrf: String,
}

enum Rejected {
    Weak(password::Weak),
    Mismatch,
    WrongCurrent,
    SameAsOld,
    Storage,
}

pub async fn password_submit(
    State(st): State<Shared>,
    Extension(client): Extension<ClientInfo>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
    Form(form): Form<PasswordForm>,
) -> Response {
    let Some(cu) = current.clone() else {
        return Redirect::to("/.cc/login?next=%2F.cc%2Fpassword").into_response();
    };
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    if !csrf_ok(&headers, &st, &cu, &form.csrf) {
        return forbidden(&ctx);
    }
    let forced = cu.must_change_password;
    let wait = st.backoff.wait(client.ip, &cu.key, now());
    let result = if wait > 0 {
        Err(None)
    } else if form.new != form.repeat {
        Err(Some(Rejected::Mismatch))
    } else if let Err(w) = password::check_new(&form.new, &cu.name) {
        Err(Some(Rejected::Weak(w)))
    } else {
        let (st2, cu2, current_pw, new_pw) = (
            st.clone(),
            cu.clone(),
            form.current.clone(),
            form.new.clone(),
        );
        tokio::task::spawn_blocking(move || -> Result<(), Rejected> {
            let mut u = st2
                .db
                .user(&cu2.key)
                .ok()
                .flatten()
                .ok_or(Rejected::Storage)?;
            if !forced && !password::verify(&current_pw, &u.password_hash) {
                return Err(Rejected::WrongCurrent);
            }
            if password::verify(&new_pw, &u.password_hash) {
                return Err(Rejected::SameAsOld);
            }
            u.password_hash = password::hash(&new_pw);
            u.must_change_password = false;
            st2.db.put_user(&u).map_err(|_| Rejected::Storage)?;
            st2.db
                .delete_user_sessions(&cu2.key, Some(&cu2.session_hash))
                .map_err(|_| Rejected::Storage)?;
            Ok(())
        })
        .await
        .unwrap_or(Err(Rejected::Storage))
        .map_err(Some)
    };

    let min = password::MIN_ACCOUNT_PASSWORD.to_string();
    let error = match result {
        Ok(()) => {
            audit(
                &st,
                format!("user:{}", cu.key),
                client.ip,
                "password_changed",
                &cu.key,
                "",
            );
            if forced {
                return Redirect::to("/").into_response();
            }
            None
        }
        Err(None) => Some(slowed_down(&ctx, wait)),
        Err(Some(r)) => Some(match r {
            Rejected::Mismatch => ctx.t("password.mismatch").to_string(),
            Rejected::Weak(password::Weak::TooShort) => {
                ctx.tf("password.too_short", &[("min", &min)])
            }
            Rejected::Weak(password::Weak::SameAsName) => {
                ctx.t("password.same_as_name").to_string()
            }
            Rejected::SameAsOld => ctx.t("password.same_as_old").to_string(),
            Rejected::WrongCurrent => {
                st.backoff
                    .fail(client.ip, &cu.key, client.origin == Origin::Local, now());
                ctx.t("password.wrong_current").to_string()
            }
            Rejected::Storage => ctx.t("error.server_error_body").to_string(),
        }),
    };
    let status = if error.is_some() {
        StatusCode::UNPROCESSABLE_ENTITY
    } else {
        StatusCode::OK
    };
    let view = PasswordView {
        forced,
        csrf: cu.csrf.clone(),
        saved: error.is_none(),
        error,
    };
    (status, views::password(&ctx, &view)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_must_stay_on_this_site() {
        assert_eq!(safe_next("/Medien/?zip"), "/Medien/?zip");
        for bad in [
            "https://evil.example",
            "//evil.example",
            "/\\evil.example",
            "",
            "/a\nb",
        ] {
            assert_eq!(safe_next(bad), "/", "{bad:?}");
        }
    }

    #[test]
    fn basic_auth_header() {
        let mut h = HeaderMap::new();
        // "jana:k3m7q-abc" and "Tibor:pä:ss"
        h.insert(
            header::AUTHORIZATION,
            "Basic amFuYTprM203cS1hYmM=".parse().unwrap(),
        );
        assert_eq!(
            basic_credentials(&h),
            Some(("jana".into(), "k3m7q-abc".into()))
        );
        h.insert(
            header::AUTHORIZATION,
            "basic VGlib3I6cMOkOnNz".parse().unwrap(),
        );
        assert_eq!(
            basic_credentials(&h),
            Some(("Tibor".into(), "pä:ss".into()))
        );
        for bad in [
            "Bearer abc",
            "Basic !!!!",
            "Basic amFuYQ==",
            "Basic a=bc",
            "Basic",
        ] {
            h.insert(header::AUTHORIZATION, bad.parse().unwrap());
            assert_eq!(basic_credentials(&h), None, "{bad}");
        }
        assert_eq!(base64_decode("").unwrap(), b"");
        assert_eq!(base64_decode("YQ==").unwrap(), b"a");
        assert_eq!(base64_decode("YWI=").unwrap(), b"ab");
        assert!(base64_decode("YQ==YQ==").is_none());
    }

    #[test]
    fn origin_rules() {
        let mut h = HeaderMap::new();
        assert!(
            same_origin(&h, "https://files.example.org"),
            "no Origin header"
        );
        h.insert(header::ORIGIN, "https://files.example.org".parse().unwrap());
        assert!(same_origin(&h, "https://files.example.org"));
        h.insert(header::ORIGIN, "https://evil.example".parse().unwrap());
        h.insert(header::HOST, "files.example.org".parse().unwrap());
        assert!(!same_origin(&h, "https://files.example.org"));
        h.insert(header::ORIGIN, "http://192.168.1.9:3923".parse().unwrap());
        h.insert(header::HOST, "192.168.1.9:3923".parse().unwrap());
        assert!(
            same_origin(&h, "https://files.example.org"),
            "direct LAN access"
        );
        h.insert(header::ORIGIN, "null".parse().unwrap());
        assert!(!same_origin(&h, "https://files.example.org"));
        assert!(token_eq("abc", "abc") && !token_eq("abc", "abd") && !token_eq("abc", "ab"));
    }
}
