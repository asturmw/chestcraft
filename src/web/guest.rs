//! Visitors without an account who entered a password: share visitors (`/s/<id>`, cookie
//! scoped to the share) and public uploaders (cookie for the whole site). Their sessions
//! live in the database for 12 hours and end when the password changes or the share or
//! upload closes.

use super::auth::{audit, now};
use super::{ClientInfo, Shared, cookie};
use crate::auth::perm::{Origin, Principal};
use crate::auth::session;
use crate::db::{GuestSession, Share, ShareLogEntry, User};
use crate::fs::path::RelPath;
use crate::health::alerts::{Msg, Severity};
use crate::shares::{self, Status};
use axum::http::HeaderMap;

/// A visitor who unlocked a share.
#[derive(Debug, Clone)]
pub struct ShareGuest {
    pub hash: String,
    pub session: GuestSession,
    pub share: Share,
    /// The creator, whose rights limit the share.
    pub owner: Principal,
}

/// A visitor who entered the public upload password of a volume.
#[derive(Debug, Clone)]
pub struct PublicGuest {
    pub hash: String,
    pub volume: String,
    pub csrf: String,
    pub max_file: Option<u64>,
    pub notify: bool,
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    tokio::task::spawn_blocking(f).await.ok()
}

fn cookie_hash(headers: &HeaderMap, name: &str) -> Option<String> {
    cookie(headers, name)
        .filter(|v| session::plausible(v))
        .map(session::hash_id)
}

/// The public uploader behind a request, if the cookie belongs to uploads that are open.
pub async fn public_guest(st: &Shared, headers: &HeaderMap) -> Option<PublicGuest> {
    let hash = cookie_hash(headers, shares::PUBLIC_COOKIE)?;
    let st = st.clone();
    blocking(move || {
        let s = st.db.guest(&hash).ok()??;
        let volume = s.scope.strip_prefix("public:")?.to_string();
        let now = now();
        if !shares::guest_valid(&s, &shares::public_scope(&volume), now) {
            return None;
        }
        let pw = st.db.public_write(&volume).ok()??;
        if !shares::public_open(&st.cfg, &pw, now) || s.created < pw.opened {
            return None;
        }
        Some(PublicGuest {
            hash,
            volume,
            csrf: s.csrf,
            max_file: pw.max_file,
            notify: pw.notify,
        })
    })
    .await
    .flatten()
}

/// A share with its state right now.
pub struct Opened {
    pub share: Share,
    pub status: Status,
    pub owner: Option<User>,
}

/// Look up a share; `Err` when the database or the filesystem failed.
pub async fn open_share(st: &Shared, id: &str) -> Result<Option<Opened>, ()> {
    if !shares::plausible_id(id) {
        return Ok(None);
    }
    let (st, id) = (st.clone(), id.to_string());
    blocking(move || -> Result<Option<Opened>, crate::db::DbError> {
        let Some(share) = st.db.share(&id)? else {
            return Ok(None);
        };
        let (status, owner) = shares::status(&st.cfg, &st.volumes, &st.db, &share, now())?;
        Ok(Some(Opened {
            share,
            status,
            owner,
        }))
    })
    .await
    .ok_or(())?
    .map_err(|e| tracing::error!("can't read a share: {e}"))
}

/// The visitor behind a request to an active share, if they entered its password.
pub async fn share_guest(st: &Shared, headers: &HeaderMap, opened: &Opened) -> Option<ShareGuest> {
    if opened.status != Status::Active {
        return None;
    }
    let owner = shares::owner_principal(opened.owner.as_ref()?);
    let hash = cookie_hash(headers, shares::SHARE_COOKIE)?;
    let scope = shares::share_scope(&opened.share.id);
    let share = opened.share.clone();
    let st = st.clone();
    blocking(move || {
        let s = st.db.guest(&hash).ok()??;
        shares::guest_valid(&s, &scope, now()).then_some(ShareGuest {
            hash,
            session: s,
            share,
            owner,
        })
    })
    .await
    .flatten()
}

pub enum Unlock {
    Ok,
    /// Wrong password; `wait` seconds before the next try counts.
    Wrong {
        wait: i64,
    },
    /// Too many wrong passwords from this address: try again in `wait` seconds.
    Slowed {
        wait: i64,
    },
}

/// Check a share or public upload password, with the same backoff as sign-ins (per address
/// and `key`, e.g. `share:<id>`).
pub async fn check_password(
    st: &Shared,
    client: &ClientInfo,
    key: &str,
    hash: String,
    passphrase: bool,
    input: String,
) -> Unlock {
    let now = now();
    let wait = st.backoff.wait(client.ip, key, now);
    if wait > 0 {
        return Unlock::Slowed { wait };
    }
    let ok = blocking(move || shares::verify_password(&input, &hash, passphrase))
        .await
        .unwrap_or(false);
    if ok {
        st.backoff.succeed(client.ip, key);
        return Unlock::Ok;
    }
    let f = st
        .backoff
        .fail(client.ip, key, client.origin == Origin::Local, now);
    if f.stuffing {
        tracing::warn!(key, "many wrong passwords from many addresses");
        audit(st, "anon".into(), client.ip, "credential_stuffing", key, "");
        super::auth::stuffing_alert(st, key);
    }
    if f.wait > 0 {
        // Slowed down for this address: tell the admin, once per six hours per target.
        let (kind, name) = key.split_once(':').unwrap_or(("share", key));
        super::raise(
            st,
            &format!("locked:{key}"),
            6 * 3600,
            Severity::Warning,
            Msg::new(if kind == "public" {
                "public_locked"
            } else {
                "share_locked"
            })
            .text("target", name)
            .text("ip", client.ip.to_string())
            .num("minutes", ((f.wait + 59) / 60) as u64),
        );
    }
    Unlock::Wrong { wait: f.wait }
}

/// Start a guest session; returns the `Set-Cookie` value.
pub async fn start(
    st: &Shared,
    client: &ClientInfo,
    scope: String,
    cookie_name: &str,
    cookie_path: &str,
) -> Option<String> {
    let g = shares::new_guest(scope, client.ip, now());
    let (st, hash, session) = (st.clone(), g.hash.clone(), g.session.clone());
    blocking(move || st.db.put_guest(&hash, &session))
        .await?
        .map_err(|e| tracing::error!("can't store a guest session: {e}"))
        .ok()?;
    Some(shares::guest_cookie(
        cookie_name,
        &g.cookie_value,
        cookie_path,
        shares::GUEST_SECS,
        client.https,
    ))
}

/// Record a visitor action in the share's access log and the audit log, without holding up
/// the request. `file` is relative to the share.
pub fn share_event(st: &Shared, share: &Share, client: &ClientInfo, action: &str, file: &str) {
    let entry = ShareLogEntry {
        time: now(),
        ip: client.ip.to_string(),
        country: client.country_code(),
        action: action.to_string(),
        file: file.to_string(),
    };
    let path = RelPath::parse(&share.path)
        .and_then(|p| p.join_path(&RelPath::parse(file)?))
        .unwrap_or_default();
    audit(
        st,
        format!("share:{}", share.id),
        client.ip,
        &format!("share_{action}"),
        &format!("{}/{}", share.volume, path.as_str()),
        &entry.country,
    );
    let (db, id) = (st.db.clone(), share.id.clone());
    tokio::task::spawn_blocking(move || {
        if let Err(e) = db.share_log(&id, &entry) {
            tracing::warn!("share log write failed: {e}");
        }
    });
}

/// Count an open, download or upload on the share (not durably).
pub async fn bump(st: &Shared, id: &str, f: impl FnOnce(&mut Share) + Send + 'static) {
    let (db, id) = (st.db.clone(), id.to_string());
    let _ = blocking(move || db.update_share(&id, false, f)).await;
}

/// Remember uploads in the visitor's session (upload-only pages list them).
pub async fn remember_uploads(st: &Shared, hash: &str, paths: Vec<String>) {
    let (db, hash) = (st.db.clone(), hash.to_string());
    let _ = blocking(move || {
        db.update_guest(&hash, |s| {
            s.uploads.extend(paths);
            let excess = s.uploads.len().saturating_sub(shares::MAX_LISTED_UPLOADS);
            s.uploads.drain(..excess);
        })
    })
    .await;
}
