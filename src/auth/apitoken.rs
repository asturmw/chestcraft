//! Tokens for the status API (`/api/v1/status`): read-only, not tied to an account, for a
//! watchdog or monitoring tool on the LAN. The admin creates them on the volumes page (or
//! with `chestcraft api-token add`), sees each once, and revokes them one by one. They are
//! random and long, so a SHA-256 is enough to store them.

use super::random;
use crate::db::{ApiToken, Db, DbError};
use sha2::{Digest, Sha256};

pub const PREFIX: &str = "ccs_";
/// 44 base58 characters: 257 bits.
const BODY_LEN: usize = 44;

pub const MAX_TOKENS: usize = 20;
pub const MAX_LABEL_CHARS: usize = 64;

/// `last_used` is written at most this often (seconds).
pub const TOUCH_EVERY: i64 = 600;

/// A new token like `ccs_7Hq…`.
pub fn generate() -> String {
    format!("{PREFIX}{}", random::string(random::BASE58, BODY_LEN))
}

/// Key under which a token is stored. Surrounding spaces from copy and paste don't count.
pub fn hash(token: &str) -> String {
    random::hex(&Sha256::digest(token.trim().as_bytes()))
}

/// Cheap shape test before touching the database.
pub fn plausible(token: &str) -> bool {
    token
        .trim()
        .strip_prefix(PREFIX)
        .is_some_and(|b| b.len() == BODY_LEN && b.bytes().all(|c| random::BASE58.contains(&c)))
}

#[derive(Debug, thiserror::Error)]
pub enum CreateError {
    #[error("labels need 1–{MAX_LABEL_CHARS} characters, without control characters")]
    BadLabel,
    #[error("there is already a token with this label")]
    Duplicate,
    #[error("there can be at most {MAX_TOKENS} tokens")]
    TooMany,
    #[error("{0}")]
    Db(#[from] DbError),
}

/// Create a token; returns the secret (shown once) and the record. `by` is `user:<key>` or
/// `cli`.
pub fn create(db: &Db, label: &str, by: &str, now: i64) -> Result<(String, ApiToken), CreateError> {
    let label = label.trim();
    let n = label.chars().count();
    if n == 0 || n > MAX_LABEL_CHARS || label.chars().any(char::is_control) {
        return Err(CreateError::BadLabel);
    }
    let existing = db.api_tokens()?;
    if existing
        .iter()
        .any(|(_, r)| r.label.to_lowercase() == label.to_lowercase())
    {
        return Err(CreateError::Duplicate);
    }
    if existing.len() >= MAX_TOKENS {
        return Err(CreateError::TooMany);
    }
    let token = generate();
    let rec = ApiToken {
        id: random::hex(&random::bytes::<6>()),
        label: label.to_string(),
        created: now,
        created_by: by.to_string(),
        last_used: None,
        last_ip: None,
    };
    db.put_api_token(&hash(&token), &rec, true)?;
    Ok((token, rec))
}

/// Revoke the token whose id or label (any case) is `which`. Returns the revoked record.
pub fn revoke(db: &Db, which: &str) -> Result<Option<ApiToken>, DbError> {
    let which = which.trim();
    let found = db
        .api_tokens()?
        .into_iter()
        .find(|(_, r)| r.id == which || r.label.to_lowercase() == which.to_lowercase());
    match found {
        Some((hash, rec)) => {
            db.delete_api_token(&hash)?;
            Ok(Some(rec))
        }
        None => Ok(None),
    }
}

/// The live token record for `token`, if any.
pub fn verify(db: &Db, token: &str) -> Result<Option<(String, ApiToken)>, DbError> {
    if !plausible(token) {
        return Ok(None);
    }
    let hash = hash(token);
    Ok(db.api_token(&hash)?.map(|rec| (hash, rec)))
}

/// Remember when and from where a token was last used (at most every [`TOUCH_EVERY`]
/// seconds, not durably).
pub fn touch(db: &Db, hash: &str, rec: &ApiToken, ip: &str, now: i64) {
    let stale =
        rec.last_used.is_none_or(|t| now - t > TOUCH_EVERY) || rec.last_ip.as_deref() != Some(ip);
    if stale {
        let rec = ApiToken {
            last_used: Some(now),
            last_ip: Some(ip.to_string()),
            ..rec.clone()
        };
        let _ = db.put_api_token(hash, &rec, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::FILE_NAME;

    fn db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join(FILE_NAME)).unwrap();
        (dir, db)
    }

    #[test]
    fn tokens_have_the_documented_shape() {
        let t = generate();
        assert_eq!(t.len(), PREFIX.len() + BODY_LEN, "{t}");
        assert!(plausible(&t) && plausible(&format!(" {t}\n")));
        assert!(!plausible(&t[PREFIX.len()..]), "prefix required");
        assert!(!plausible(&format!("{t}x")) && !plausible("ccs_0OIl"));
        assert_ne!(generate(), t);
        assert_eq!(hash(&t), hash(&format!("{t} ")));
    }

    #[test]
    fn create_verify_touch_revoke() {
        let (_d, db) = db();
        let (token, rec) = create(&db, " Watchdog ", "user:tibor", 10).unwrap();
        assert_eq!(
            (rec.label.as_str(), rec.created_by.as_str()),
            ("Watchdog", "user:tibor")
        );
        assert!(matches!(
            create(&db, "WATCHDOG", "cli", 11),
            Err(CreateError::Duplicate)
        ));
        assert!(matches!(
            create(&db, " ", "cli", 11),
            Err(CreateError::BadLabel)
        ));
        assert!(matches!(
            create(&db, "a\nb", "cli", 11),
            Err(CreateError::BadLabel)
        ));

        let (hash, found) = verify(&db, &token).unwrap().unwrap();
        assert_eq!(found, rec);
        assert!(verify(&db, &generate()).unwrap().is_none());
        assert!(verify(&db, "Bearer").unwrap().is_none());
        touch(&db, &hash, &found, "192.0.2.5", 100);
        let stored = db.api_token(&hash).unwrap().unwrap();
        assert_eq!(
            (stored.last_used, stored.last_ip.as_deref()),
            (Some(100), Some("192.0.2.5"))
        );
        // Within TOUCH_EVERY from the same address: no write.
        touch(&db, &hash, &stored, "192.0.2.5", 200);
        assert_eq!(db.api_token(&hash).unwrap().unwrap().last_used, Some(100));

        assert_eq!(revoke(&db, "watchdog").unwrap().unwrap().id, rec.id);
        assert!(verify(&db, &token).unwrap().is_none());
        assert!(revoke(&db, &rec.id).unwrap().is_none());
    }

    #[test]
    fn at_most_twenty() {
        let (_d, db) = db();
        for i in 0..MAX_TOKENS {
            create(&db, &format!("tool {i}"), "cli", i as i64).unwrap();
        }
        assert!(matches!(
            create(&db, "one more", "cli", 99),
            Err(CreateError::TooMany)
        ));
        let labels: Vec<String> = db
            .api_tokens()
            .unwrap()
            .into_iter()
            .map(|(_, r)| r.label)
            .collect();
        assert_eq!(
            labels.first().map(String::as_str),
            Some("tool 0"),
            "oldest first"
        );
    }
}
