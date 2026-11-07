//! Account management shared by the CLI (`chestcraft user …`) and the admin's users page:
//! create a person with a one-time password, reset it, change the role, block and unblock.
//! Agents are created in [`super::agent`]; role and block changes work for both kinds.

use super::perm::ADMIN_ROLE;
use crate::config::Config;
use crate::db::{Db, DbError, User};

#[derive(Debug, thiserror::Error)]
pub enum AccountError {
    #[error("\"{0}\": use 1–32 of a–z, A–Z, 0–9, . _ - (not starting with . or -)")]
    BadName(String),
    #[error("unknown role \"{0}\" (admin or one from [roles])")]
    UnknownRole(String),
    #[error("an account named \"{0}\" exists already")]
    Exists(String),
    #[error("no account named \"{0}\"")]
    NoAccount(String),
    #[error("{0} is an agent: it has no password; give it a token instead")]
    IsAgent(String),
    #[error("{0} is an agent; agents can't have the admin role")]
    AgentAdmin(String),
    #[error("{0}")]
    Db(#[from] DbError),
}

/// "admin" or a role from the config.
pub fn role_exists(cfg: &Config, role: &str) -> bool {
    role == ADMIN_ROLE || cfg.roles.contains_key(role)
}

/// A new person who must pick a password at the first sign-in; returns the one-time
/// password. Never touches an existing account.
pub fn create_person(
    db: &Db,
    cfg: &Config,
    name: &str,
    role: &str,
    now: i64,
) -> Result<(User, String), AccountError> {
    let name = name.trim();
    if !super::valid_username(name) {
        return Err(AccountError::BadName(name.to_string()));
    }
    if !role_exists(cfg, role) {
        return Err(AccountError::UnknownRole(role.to_string()));
    }
    let pw = super::password::generate();
    let user = User {
        name: name.to_string(),
        role: role.to_string(),
        password_hash: super::password::hash(&pw),
        must_change_password: true,
        disabled: false,
        created: now,
        last_login: None,
        settings: Default::default(),
        known_remote_ips: vec![],
        kind: Default::default(),
    };
    if !db.insert_user(&user)? {
        return Err(AccountError::Exists(name.to_string()));
    }
    Ok((user, pw))
}

/// A new one-time password; the account's sessions end.
pub fn reset_password(db: &Db, name: &str) -> Result<(User, String), AccountError> {
    let user = db
        .user(name)?
        .ok_or_else(|| AccountError::NoAccount(name.trim().to_string()))?;
    if user.is_agent() {
        return Err(AccountError::IsAgent(user.name));
    }
    let pw = super::password::generate();
    let hash = super::password::hash(&pw);
    let user = db
        .update_user(&user.name, |u| {
            u.password_hash = hash;
            u.must_change_password = true;
        })?
        .ok_or_else(|| AccountError::NoAccount(name.trim().to_string()))?;
    db.delete_user_sessions(&User::key(&user.name), None)?;
    Ok((user, pw))
}

pub fn set_role(db: &Db, cfg: &Config, name: &str, role: &str) -> Result<User, AccountError> {
    let user = db
        .user(name)?
        .ok_or_else(|| AccountError::NoAccount(name.trim().to_string()))?;
    if !role_exists(cfg, role) {
        return Err(AccountError::UnknownRole(role.to_string()));
    }
    if user.is_agent() && role == ADMIN_ROLE {
        return Err(AccountError::AgentAdmin(user.name));
    }
    db.update_user(&user.name, |u| u.role = role.to_string())?
        .ok_or_else(|| AccountError::NoAccount(name.trim().to_string()))
}

/// Block or unblock; blocking ends the account's sessions (returned: how many).
pub fn set_disabled(db: &Db, name: &str, disabled: bool) -> Result<(User, usize), AccountError> {
    let user = db
        .update_user(name, |u| u.disabled = disabled)?
        .ok_or_else(|| AccountError::NoAccount(name.trim().to_string()))?;
    let ended = if disabled {
        db.delete_user_sessions(&User::key(&user.name), None)?
    } else {
        0
    };
    Ok((user, ended))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, Db, Config) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join(crate::db::FILE_NAME)).unwrap();
        let cfg = Config::from_toml_str(
            "[server]\npublic_url = \"https://files.example.org\"\n[volumes.Docs]\npath = \"/srv/docs\"\n[roles.leser]\nDocs = \"r\"\n",
        )
        .unwrap();
        (dir, db, cfg)
    }

    #[test]
    fn create_reset_role_block() {
        let (_d, db, cfg) = setup();
        let (u, pw) = create_person(&db, &cfg, " Jana ", "leser", 5).unwrap();
        assert_eq!(u.name, "Jana");
        assert!(u.must_change_password);
        assert!(crate::auth::password::verify(&pw, &u.password_hash));
        assert!(matches!(
            create_person(&db, &cfg, "JANA", "admin", 6),
            Err(AccountError::Exists(_))
        ));
        assert!(matches!(
            create_person(&db, &cfg, "kim", "chef", 6),
            Err(AccountError::UnknownRole(_))
        ));
        assert!(matches!(
            create_person(&db, &cfg, ".x", "leser", 6),
            Err(AccountError::BadName(_))
        ));

        let (_, pw2) = reset_password(&db, "jana").unwrap();
        let stored = db.user("jana").unwrap().unwrap();
        assert!(crate::auth::password::verify(&pw2, &stored.password_hash));
        assert!(!crate::auth::password::verify(&pw, &stored.password_hash));

        assert_eq!(set_role(&db, &cfg, "jana", "admin").unwrap().role, "admin");
        assert!(set_disabled(&db, "jana", true).unwrap().0.disabled);
        assert!(!set_disabled(&db, "jana", false).unwrap().0.disabled);
        assert!(matches!(
            set_disabled(&db, "nobody", true),
            Err(AccountError::NoAccount(_))
        ));

        let a = crate::auth::agent::create(&db, &cfg, "bot", "leser", 7).unwrap();
        assert!(matches!(
            reset_password(&db, &a.name),
            Err(AccountError::IsAgent(_))
        ));
        assert!(matches!(
            set_role(&db, &cfg, "bot", "admin"),
            Err(AccountError::AgentAdmin(_))
        ));
    }
}
