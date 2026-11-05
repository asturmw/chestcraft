//! Browser sessions: a random cookie value, stored under its SHA-256 so a copy of the
//! database doesn't hold live sessions.

use super::perm::Origin;
use super::random;
use crate::config::SessionsConfig;
use crate::db::Session;
use sha2::{Digest, Sha256};
use std::net::IpAddr;

pub const COOKIE: &str = "cc_session";

/// `last_seen` is written at most this often (seconds).
pub const TOUCH_EVERY: i64 = 600;

pub struct NewSession {
    pub cookie_value: String,
    pub id_hash: String,
    pub session: Session,
}

pub fn create(user_key: &str, ip: IpAddr, remote: bool, now: i64) -> NewSession {
    let cookie_value = random::hex(&random::bytes::<32>());
    NewSession {
        id_hash: hash_id(&cookie_value),
        cookie_value,
        session: Session {
            user: user_key.to_string(),
            created: now,
            last_seen: now,
            csrf: random::hex(&random::bytes::<16>()),
            ip: ip.to_string(),
            remote,
        },
    }
}

pub fn hash_id(cookie_value: &str) -> String {
    random::hex(&Sha256::digest(cookie_value.as_bytes()))
}

/// Cookie values we hand out are 64 hex characters; anything else is ignored.
pub fn plausible(cookie_value: &str) -> bool {
    cookie_value.len() == 64 && cookie_value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Sessions end after `user_days` without use. An admin session used from outside also
/// ends `remote_admin_hours` after sign-in.
pub fn is_valid(s: &Session, now: i64, cfg: &SessionsConfig, admin: bool, origin: Origin) -> bool {
    if now - s.last_seen > i64::from(cfg.user_days) * 86_400 {
        return false;
    }
    !(admin
        && origin == Origin::Remote
        && now - s.created > i64::from(cfg.remote_admin_hours) * 3600)
}

/// Cookie lifetime for a new session.
pub fn max_age(cfg: &SessionsConfig, admin: bool, origin: Origin) -> i64 {
    if admin && origin == Origin::Remote {
        i64::from(cfg.remote_admin_hours) * 3600
    } else {
        i64::from(cfg.user_days) * 86_400
    }
}

pub fn set_cookie(value: &str, max_age: i64, secure: bool) -> String {
    let secure = if secure { "; Secure" } else { "" };
    format!("{COOKIE}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn idle_and_remote_admin_expiry() {
        let cfg = Config::from_toml_str("[server]\npublic_url = \"http://x.org\"\n")
            .unwrap()
            .sessions;
        let s = create("tibor", "192.168.1.2".parse().unwrap(), false, 0).session;
        let day = 86_400;
        assert!(is_valid(&s, 29 * day, &cfg, false, Origin::Remote));
        assert!(!is_valid(&s, 31 * day, &cfg, false, Origin::Local));
        assert!(is_valid(&s, 13 * 3600, &cfg, true, Origin::Local));
        assert!(!is_valid(&s, 13 * 3600, &cfg, true, Origin::Remote));
        assert_eq!(max_age(&cfg, true, Origin::Remote), 12 * 3600);
        assert_eq!(max_age(&cfg, false, Origin::Remote), 30 * day);
    }

    #[test]
    fn ids_are_random_and_hashed() {
        let a = create("x", "::1".parse().unwrap(), false, 0);
        let b = create("x", "::1".parse().unwrap(), false, 0);
        assert!(plausible(&a.cookie_value));
        assert_ne!(a.cookie_value, b.cookie_value);
        assert_eq!(a.id_hash, hash_id(&a.cookie_value));
        assert_ne!(a.id_hash, a.cookie_value);
        assert_ne!(a.session.csrf, b.session.csrf);
        assert!(!plausible("../etc") && !plausible(&"z".repeat(64)));
        assert!(set_cookie("v", 60, true).ends_with("Max-Age=60; Secure"));
    }
}
