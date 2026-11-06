//! Agent accounts and their bearer tokens for the MCP server.
//!
//! An agent is an account of kind [`Kind::Agent`]: a name and one role from the config,
//! never the built-in admin role, no password, no web or WebDAV sign-in. It reaches
//! chestcraft only through the MCP server, with `Authorization: Bearer cca_…`. Tokens are
//! random (257 bits), stored as SHA-256, shown once, and carry a label (one per host), an
//! optional expiry and when/where they were last used. A disabled agent's tokens stop
//! working; its shares pause like any disabled account's.

use super::random;
use crate::config::Config;
use crate::db::{AgentToken, Db, DbError, Kind, User};
use sha2::{Digest, Sha256};

pub const PREFIX: &str = "cca_";
/// 44 base58 characters: 257 bits.
const BODY_LEN: usize = 44;

pub const MAX_TOKENS_PER_AGENT: usize = 20;
pub const MAX_LABEL_CHARS: usize = 64;

/// `last_used` is written at most this often (seconds); the dashboard shows it as the
/// agent's last call.
pub const TOUCH_EVERY: i64 = 60;

/// A new token like `cca_7Hq…`.
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
pub enum AgentError {
    #[error("\"{0}\": use 1–32 of a–z, A–Z, 0–9, . _ - (not starting with . or -)")]
    BadName(String),
    #[error("agents can't have the admin role")]
    AdminRole,
    #[error("unknown role \"{0}\" (agents need a role from [roles])")]
    UnknownRole(String),
    #[error("an account named \"{0}\" exists already")]
    Exists(String),
    #[error("no agent named \"{0}\"")]
    NoAgent(String),
    #[error("labels need 1–{MAX_LABEL_CHARS} characters, without control characters")]
    BadLabel,
    #[error("this agent already has a token with this label")]
    Duplicate,
    #[error("an agent can have at most {MAX_TOKENS_PER_AGENT} tokens")]
    TooMany,
    #[error("the expiry is in the past")]
    Expired,
    #[error("{0}")]
    Db(#[from] DbError),
}

/// May an agent have `role`?
pub fn check_role(cfg: &Config, role: &str) -> Result<(), AgentError> {
    if role == super::perm::ADMIN_ROLE {
        return Err(AgentError::AdminRole);
    }
    if !cfg.roles.contains_key(role) {
        return Err(AgentError::UnknownRole(role.to_string()));
    }
    Ok(())
}

/// Create an agent account (never overwrites an account).
pub fn create(db: &Db, cfg: &Config, name: &str, role: &str, now: i64) -> Result<User, AgentError> {
    let name = name.trim();
    if !super::valid_username(name) {
        return Err(AgentError::BadName(name.to_string()));
    }
    check_role(cfg, role)?;
    let user = User {
        name: name.to_string(),
        role: role.to_string(),
        // Never a valid PHC string, so no password can ever match.
        password_hash: String::new(),
        must_change_password: false,
        disabled: false,
        created: now,
        last_login: None,
        settings: Default::default(),
        known_remote_ips: vec![],
        kind: Kind::Agent,
    };
    if !db.insert_user(&user)? {
        return Err(AgentError::Exists(name.to_string()));
    }
    Ok(user)
}

/// The agent account `name`, if it is one.
pub fn get(db: &Db, name: &str) -> Result<User, AgentError> {
    db.user(name)?
        .filter(User::is_agent)
        .ok_or_else(|| AgentError::NoAgent(name.trim().to_string()))
}

/// Tokens of one agent, oldest first.
pub fn tokens_of(db: &Db, agent_key: &str) -> Result<Vec<(String, AgentToken)>, DbError> {
    Ok(db
        .agent_tokens()?
        .into_iter()
        .filter(|(_, t)| t.agent == agent_key)
        .collect())
}

/// Create a token for the agent `name`; returns the secret (shown once) and the record.
/// `by` is `user:<key>` or `cli`.
pub fn create_token(
    db: &Db,
    name: &str,
    label: &str,
    expires: Option<i64>,
    by: &str,
    now: i64,
) -> Result<(String, AgentToken), AgentError> {
    let agent = get(db, name)?;
    let label = label.trim();
    let n = label.chars().count();
    if n == 0 || n > MAX_LABEL_CHARS || label.chars().any(char::is_control) {
        return Err(AgentError::BadLabel);
    }
    if expires.is_some_and(|t| t <= now) {
        return Err(AgentError::Expired);
    }
    let key = User::key(&agent.name);
    let existing = tokens_of(db, &key)?;
    if existing
        .iter()
        .any(|(_, r)| r.label.to_lowercase() == label.to_lowercase())
    {
        return Err(AgentError::Duplicate);
    }
    if existing.len() >= MAX_TOKENS_PER_AGENT {
        return Err(AgentError::TooMany);
    }
    let token = generate();
    let rec = AgentToken {
        id: random::hex(&random::bytes::<6>()),
        agent: key,
        label: label.to_string(),
        created: now,
        created_by: by.to_string(),
        expires,
        last_used: None,
        last_ip: None,
    };
    db.put_agent_token(&hash(&token), &rec, true)?;
    Ok((token, rec))
}

/// Revoke the token of `agent_key` whose id or label (any case) is `which`.
pub fn revoke(db: &Db, agent_key: &str, which: &str) -> Result<Option<AgentToken>, DbError> {
    let which = which.trim();
    let found = tokens_of(db, agent_key)?
        .into_iter()
        .find(|(_, r)| r.id == which || r.label.to_lowercase() == which.to_lowercase());
    match found {
        Some((hash, rec)) => {
            db.delete_agent_token(&hash)?;
            Ok(Some(rec))
        }
        None => Ok(None),
    }
}

/// A token that works right now, with its agent.
#[derive(Debug, Clone)]
pub struct Verified {
    pub agent: User,
    pub token: AgentToken,
    pub hash: String,
}

/// The agent behind `token`, if the token is live and the agent may work: an enabled agent
/// account whose role isn't admin.
pub fn verify(db: &Db, token: &str, now: i64) -> Result<Option<Verified>, DbError> {
    if !plausible(token) {
        return Ok(None);
    }
    let hash = hash(token);
    let Some(rec) = db.agent_token(&hash)? else {
        return Ok(None);
    };
    if rec.expires.is_some_and(|t| t <= now) {
        return Ok(None);
    }
    let Some(agent) = db.user(&rec.agent)? else {
        return Ok(None);
    };
    if !agent.is_agent() || agent.disabled || agent.role == super::perm::ADMIN_ROLE {
        return Ok(None);
    }
    Ok(Some(Verified {
        agent,
        token: rec,
        hash,
    }))
}

/// Remember when and from where a token was last used (at most every [`TOUCH_EVERY`]
/// seconds, not durably).
pub fn touch(db: &Db, v: &Verified, ip: &str, now: i64) {
    let stale = v.token.last_used.is_none_or(|t| now - t >= TOUCH_EVERY)
        || v.token.last_ip.as_deref() != Some(ip);
    if stale {
        let rec = AgentToken {
            last_used: Some(now),
            last_ip: Some(ip.to_string()),
            ..v.token.clone()
        };
        let _ = db.put_agent_token(&v.hash, &rec, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::FILE_NAME;

    fn setup() -> (tempfile::TempDir, Db, Config) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join(FILE_NAME)).unwrap();
        let cfg = Config::from_toml_str(&format!(
            "[server]\npublic_url = \"https://x.org\"\n[volumes.Docs]\npath = \"{}\"\n\
             [roles.bots]\nDocs = \"r\"\n",
            dir.path().display()
        ))
        .unwrap();
        (dir, db, cfg)
    }

    #[test]
    fn tokens_have_the_documented_shape() {
        let t = generate();
        assert_eq!(t.len(), PREFIX.len() + BODY_LEN, "{t}");
        assert!(plausible(&t) && plausible(&format!(" {t}\n")));
        assert!(!plausible(&t[PREFIX.len()..]), "prefix required");
        assert!(
            !plausible(&format!("ccs_{}", &t[PREFIX.len()..])),
            "not a status token"
        );
        assert!(!plausible(&format!("{t}x")));
        assert_eq!(hash(&t), hash(&format!("{t} ")));
    }

    #[test]
    fn agents_need_a_role_and_never_admin() {
        let (_d, db, cfg) = setup();
        assert!(matches!(
            create(&db, &cfg, "hermes", "admin", 1),
            Err(AgentError::AdminRole)
        ));
        assert!(matches!(
            create(&db, &cfg, "hermes", "nope", 1),
            Err(AgentError::UnknownRole(_))
        ));
        assert!(matches!(
            create(&db, &cfg, "-x", "bots", 1),
            Err(AgentError::BadName(_))
        ));
        let a = create(&db, &cfg, "Hermes", "bots", 1).unwrap();
        assert!(a.is_agent() && a.password_hash.is_empty());
        assert!(!crate::auth::password::verify("", &a.password_hash));
        assert!(matches!(
            create(&db, &cfg, "hermes", "bots", 2),
            Err(AgentError::Exists(_))
        ));
        // A person's account is not an agent.
        let mut person = a.clone();
        person.name = "kim".into();
        person.kind = Kind::Person;
        db.put_user(&person).unwrap();
        assert!(matches!(get(&db, "kim"), Err(AgentError::NoAgent(_))));
        assert!(matches!(
            create_token(&db, "kim", "x", None, "cli", 3),
            Err(AgentError::NoAgent(_))
        ));
    }

    #[test]
    fn tokens_verify_until_revoked_expired_or_disabled() {
        let (_d, db, cfg) = setup();
        create(&db, &cfg, "hermes", "bots", 1).unwrap();
        let (token, rec) = create_token(&db, "HERMES", " desktop ", None, "cli", 10).unwrap();
        assert_eq!(
            (rec.label.as_str(), rec.agent.as_str()),
            ("desktop", "hermes")
        );
        assert!(matches!(
            create_token(&db, "hermes", "DESKTOP", None, "cli", 11),
            Err(AgentError::Duplicate)
        ));
        assert!(matches!(
            create_token(&db, "hermes", "old", Some(5), "cli", 11),
            Err(AgentError::Expired)
        ));
        let v = verify(&db, &token, 20).unwrap().unwrap();
        assert_eq!(v.agent.name, "hermes");
        touch(&db, &v, "192.0.2.7", 20);
        let stored = db.agent_token(&v.hash).unwrap().unwrap();
        assert_eq!(stored.last_used, Some(20));
        assert!(verify(&db, &generate(), 20).unwrap().is_none());

        // Expiry.
        let (short, _) = create_token(&db, "hermes", "short", Some(100), "cli", 20).unwrap();
        assert!(verify(&db, &short, 99).unwrap().is_some());
        assert!(verify(&db, &short, 100).unwrap().is_none());

        // Disabled agent: every token stops; enabled again: they work again.
        db.update_user("hermes", |u| u.disabled = true).unwrap();
        assert!(verify(&db, &token, 30).unwrap().is_none());
        db.update_user("hermes", |u| u.disabled = false).unwrap();
        assert!(verify(&db, &token, 30).unwrap().is_some());

        // A role edited to admin by hand never works.
        db.update_user("hermes", |u| u.role = "admin".into())
            .unwrap();
        assert!(verify(&db, &token, 30).unwrap().is_none());
        db.update_user("hermes", |u| u.role = "bots".into())
            .unwrap();

        assert_eq!(
            revoke(&db, "hermes", "DESKTOP").unwrap().unwrap().id,
            rec.id
        );
        assert!(verify(&db, &token, 40).unwrap().is_none());
        assert!(revoke(&db, "hermes", &rec.id).unwrap().is_none());
    }
}
