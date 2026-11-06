//! App passwords: per-device secrets for WebDAV (Basic auth). They're random and long, so a
//! SHA-256 is enough to store them (no argon2 on every request), and they can be revoked one
//! by one. An app password works only for its account, only while the account is enabled,
//! and never for the web sign-in.

use super::random;
use crate::db::{AppPassword, Db, DbError, User};
use sha2::{Digest, Sha256};

/// Lowercase letters and digits without l, o, 0 and 1: 32 symbols, 5 bits each.
const ALPHABET: &[u8] = b"abcdefghijkmnpqrstuvwxyz23456789";
const GROUPS: usize = 6;
const GROUP_LEN: usize = 5;

pub const MAX_PER_USER: usize = 20;
pub const MAX_LABEL_CHARS: usize = 64;

/// `last_used` is written at most this often (seconds).
pub const TOUCH_EVERY: i64 = 600;

/// A new secret like `k3m7q-...` (six groups of five, 150 bits).
pub fn generate() -> String {
    let raw = random::string(ALPHABET, GROUPS * GROUP_LEN);
    raw.as_bytes()
        .chunks(GROUP_LEN)
        .map(|c| std::str::from_utf8(c).expect("ascii"))
        .collect::<Vec<_>>()
        .join("-")
}

/// Key under which a secret is stored. Surrounding spaces from copy and paste don't count.
pub fn hash(secret: &str) -> String {
    random::hex(&Sha256::digest(secret.trim().as_bytes()))
}

/// Cheap shape test before touching the database.
pub fn plausible(secret: &str) -> bool {
    let s = secret.trim();
    s.len() == GROUPS * GROUP_LEN + GROUPS - 1
        && s.bytes().all(|b| ALPHABET.contains(&b) || b == b'-')
}

#[derive(Debug, thiserror::Error)]
pub enum CreateError {
    #[error("labels need 1–{MAX_LABEL_CHARS} characters, without control characters")]
    BadLabel,
    #[error("there is already an app password with this label")]
    Duplicate,
    #[error("an account can have at most {MAX_PER_USER} app passwords")]
    TooMany,
    #[error("no such account")]
    NoUser,
    #[error("agents use MCP tokens, not app passwords")]
    Agent,
    #[error("{0}")]
    Db(#[from] DbError),
}

pub fn clean_label(label: &str) -> Result<String, CreateError> {
    let label = label.trim();
    let n = label.chars().count();
    if n == 0 || n > MAX_LABEL_CHARS || label.chars().any(char::is_control) {
        return Err(CreateError::BadLabel);
    }
    Ok(label.to_string())
}

/// Create an app password for `user_name`; returns the secret (shown once) and the record.
pub fn create(
    db: &Db,
    user_name: &str,
    label: &str,
    now: i64,
) -> Result<(String, AppPassword), CreateError> {
    let label = clean_label(label)?;
    let user = db.user(user_name)?.ok_or(CreateError::NoUser)?;
    if user.is_agent() {
        return Err(CreateError::Agent);
    }
    let key = User::key(&user.name);
    let existing = db.app_passwords_of(&key)?;
    if existing
        .iter()
        .any(|(_, r)| r.label.to_lowercase() == label.to_lowercase())
    {
        return Err(CreateError::Duplicate);
    }
    if existing.len() >= MAX_PER_USER {
        return Err(CreateError::TooMany);
    }
    let secret = generate();
    let rec = AppPassword {
        id: random::hex(&random::bytes::<6>()),
        user: key,
        label,
        created: now,
        last_used: None,
        last_ip: None,
    };
    db.put_app_password(&hash(&secret), &rec, true)?;
    Ok((secret, rec))
}

/// Revoke the app password of `user_key` whose id or label (any case) is `which`. Returns
/// the revoked record.
pub fn revoke(db: &Db, user_key: &str, which: &str) -> Result<Option<AppPassword>, DbError> {
    let which = which.trim();
    let found = db
        .app_passwords_of(user_key)?
        .into_iter()
        .find(|(_, r)| r.id == which || r.label.to_lowercase() == which.to_lowercase());
    match found {
        Some((hash, rec)) => {
            db.delete_app_password(&hash)?;
            Ok(Some(rec))
        }
        None => Ok(None),
    }
}

/// The account behind a WebDAV sign-in, if `secret` is a live app password of `user_name`.
pub struct Verified {
    pub user: User,
    pub pass: AppPassword,
    pub hash: String,
}

pub fn verify(db: &Db, user_name: &str, secret: &str) -> Result<Option<Verified>, DbError> {
    if !plausible(secret) {
        return Ok(None);
    }
    let hash = hash(secret);
    let Some(pass) = db.app_password(&hash)? else {
        return Ok(None);
    };
    if pass.user != User::key(user_name.trim()) {
        return Ok(None);
    }
    let Some(user) = db.user(&pass.user)? else {
        return Ok(None);
    };
    // A reset password (must change) also stops WebDAV until the owner signs in again.
    if user.disabled || user.must_change_password || user.is_agent() {
        return Ok(None);
    }
    Ok(Some(Verified { user, pass, hash }))
}

/// Remember when and from where an app password was last used (at most every
/// [`TOUCH_EVERY`] seconds, not durably).
pub fn touch(db: &Db, v: &Verified, ip: &str, now: i64) {
    let stale = v.pass.last_used.is_none_or(|t| now - t > TOUCH_EVERY)
        || v.pass.last_ip.as_deref() != Some(ip);
    if stale {
        let rec = AppPassword {
            last_used: Some(now),
            last_ip: Some(ip.to_string()),
            ..v.pass.clone()
        };
        let _ = db.put_app_password(&v.hash, &rec, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{FILE_NAME, UserSettings};

    fn db_with(names: &[&str]) -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join(FILE_NAME)).unwrap();
        for n in names {
            db.put_user(&User {
                name: n.to_string(),
                role: "admin".into(),
                password_hash: "x".into(),
                must_change_password: false,
                disabled: false,
                created: 1,
                last_login: None,
                settings: UserSettings::default(),
                known_remote_ips: vec![],
                kind: Default::default(),
            })
            .unwrap();
        }
        (dir, db)
    }

    #[test]
    fn secrets_have_the_documented_shape() {
        let s = generate();
        assert_eq!(s.len(), 35, "{s}");
        assert_eq!(s.matches('-').count(), 5);
        assert!(plausible(&s) && plausible(&format!(" {s}\n")));
        assert!(!plausible("short") && !plausible(&s.to_uppercase()));
        assert_ne!(generate(), s);
        assert_eq!(hash(&s), hash(&format!("{s} ")));
    }

    #[test]
    fn create_verify_revoke() {
        let (_d, db) = db_with(&["Tibor", "jana"]);
        let (secret, rec) = create(&db, "tibor", " Workstation ", 10).unwrap();
        assert_eq!(
            (rec.user.as_str(), rec.label.as_str()),
            ("tibor", "Workstation")
        );
        assert!(matches!(
            create(&db, "Tibor", "workstation", 11),
            Err(CreateError::Duplicate)
        ));
        assert!(matches!(
            create(&db, "tibor", "", 11),
            Err(CreateError::BadLabel)
        ));
        assert!(matches!(
            create(&db, "nobody", "x", 11),
            Err(CreateError::NoUser)
        ));

        let v = verify(&db, "TIBOR", &secret).unwrap().unwrap();
        assert_eq!(v.user.name, "Tibor");
        assert!(
            verify(&db, "jana", &secret).unwrap().is_none(),
            "other account"
        );
        assert!(verify(&db, "tibor", &generate()).unwrap().is_none());
        touch(&db, &v, "192.0.2.5", 100);
        let stored = db.app_password(&hash(&secret)).unwrap().unwrap();
        assert_eq!(stored.last_used, Some(100));

        let mut u = db.user("tibor").unwrap().unwrap();
        u.disabled = true;
        db.put_user(&u).unwrap();
        assert!(verify(&db, "tibor", &secret).unwrap().is_none(), "disabled");
        u.disabled = false;
        u.must_change_password = true;
        db.put_user(&u).unwrap();
        assert!(verify(&db, "tibor", &secret).unwrap().is_none(), "reset");
        u.must_change_password = false;
        db.put_user(&u).unwrap();

        assert_eq!(
            revoke(&db, "tibor", "WORKSTATION").unwrap().unwrap().id,
            rec.id
        );
        assert!(verify(&db, "tibor", &secret).unwrap().is_none());
        assert!(revoke(&db, "tibor", &rec.id).unwrap().is_none());
    }

    #[test]
    fn at_most_twenty_per_account() {
        let (_d, db) = db_with(&["jana"]);
        for i in 0..MAX_PER_USER {
            create(&db, "jana", &format!("device {i}"), 1).unwrap();
        }
        assert!(matches!(
            create(&db, "jana", "one more", 1),
            Err(CreateError::TooMany)
        ));
    }
}
