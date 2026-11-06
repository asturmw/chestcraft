//! `chestcraft import-copyparty`: carry the accounts of a copyparty config file over.
//!
//! Each account keeps its copyparty password (stored as an argon2id hash) and must pick a
//! new one at its first sign-in. The import never changes an existing account: a name that
//! exists already is skipped, so it can simply run again after a fix. The admin gives every
//! account a role (`--role name=role`) or leaves it out (`--skip name`); the report shows
//! what each account could do in copyparty next to what its role allows in chestcraft.
//! The copyparty file is only read.

pub mod copyparty;

use crate::auth::password;
use crate::auth::perm::{self, ADMIN_ROLE, Origin, Perms, Principal};
use crate::config::Config;
use crate::db::{AuditEvent, Db, DbError, User};
use copyparty::{Access, Parsed, Password};
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::Path;

/// The admin's decisions, by copyparty account name (any case).
#[derive(Debug, Default, Clone)]
pub struct Choices {
    pub roles: Vec<(String, String)>,
    pub skip: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skip {
    NoPassword,
    BadName,
    /// Same name as an earlier account apart from case; chestcraft names ignore case.
    Duplicate(String),
    Requested,
    Exists,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Import { role: String },
    Skip(Vec<Skip>),
    NeedsRole,
}

/// One volume in the comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// copyparty volume URL (without slashes) and the account's access there.
    pub copyparty: Option<(String, Access)>,
    /// The matching chestcraft volume and what the role allows there from the LAN; `None`
    /// when no chestcraft volume matches (or no role is chosen yet).
    pub chestcraft: Option<(String, Perms)>,
}

impl Row {
    /// Read, write, move or delete differ (sharing has no copyparty counterpart).
    pub fn differs(&self) -> bool {
        let cp = self.copyparty.as_ref().map(|(_, a)| *a).unwrap_or_default();
        let cc = self
            .chestcraft
            .as_ref()
            .map(|(_, p)| *p)
            .unwrap_or_default();
        cp.read != cc.contains(Perms::READ)
            || cp.write != cc.contains(Perms::WRITE)
            || cp.moves != cc.contains(Perms::MOVE)
            || cp.delete != cc.contains(Perms::DELETE)
    }
}

#[derive(Debug)]
pub struct Entry {
    pub name: String,
    pub password: Password,
    pub action: Action,
    pub rows: Vec<Row>,
}

#[derive(Debug)]
pub struct Plan {
    pub entries: Vec<Entry>,
    pub warnings: Vec<String>,
    pub volumes: usize,
}

impl Plan {
    pub fn needs_role(&self) -> Vec<&str> {
        self.entries
            .iter()
            .filter(|e| e.action == Action::NeedsRole)
            .map(|e| e.name.as_str())
            .collect()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error("there is no account \"{0}\" in the copyparty config")]
    UnknownAccount(String),
    #[error("unknown role \"{role}\" for {name} (admin or one from [roles])")]
    UnknownRole { name: String, role: String },
    #[error("\"{0}\" is named more than once in --role/--skip")]
    Twice(String),
    #[error(transparent)]
    Db(#[from] DbError),
}

/// Decide what happens to each account. Reads the database, changes nothing.
pub fn plan(parsed: &Parsed, cfg: &Config, db: &Db, choices: &Choices) -> Result<Plan, PlanError> {
    let mut named = BTreeSet::new();
    let chosen = choices
        .roles
        .iter()
        .map(|(n, _)| n)
        .chain(choices.skip.iter());
    for name in chosen {
        if !parsed
            .accounts
            .iter()
            .any(|a| User::key(&a.name) == User::key(name))
        {
            return Err(PlanError::UnknownAccount(name.clone()));
        }
        if !named.insert(User::key(name)) {
            return Err(PlanError::Twice(name.clone()));
        }
    }
    for (name, role) in &choices.roles {
        if role != ADMIN_ROLE && !cfg.roles.contains_key(role) {
            return Err(PlanError::UnknownRole {
                name: name.clone(),
                role: role.clone(),
            });
        }
    }

    let mut seen: Vec<&str> = Vec::new();
    let mut entries = Vec::new();
    for acc in &parsed.accounts {
        let key = User::key(&acc.name);
        let mut skip = Vec::new();
        if acc.password == Password::Empty {
            skip.push(Skip::NoPassword);
        }
        if !crate::auth::valid_username(&acc.name) {
            skip.push(Skip::BadName);
        }
        if let Some(first) = seen.iter().find(|s| User::key(s) == key) {
            skip.push(Skip::Duplicate(first.to_string()));
        }
        if choices.skip.iter().any(|s| User::key(s) == key) {
            skip.push(Skip::Requested);
        }
        if db.user(&acc.name)?.is_some() {
            skip.push(Skip::Exists);
        }
        seen.push(&acc.name);
        let role = choices
            .roles
            .iter()
            .find(|(n, _)| User::key(n) == key)
            .map(|(_, r)| r.clone());
        let (action, rows) = if !skip.is_empty() {
            (Action::Skip(skip), Vec::new())
        } else {
            let rows = rows(parsed, cfg, &acc.name, role.as_deref());
            match role {
                Some(role) => (Action::Import { role }, rows),
                None => (Action::NeedsRole, rows),
            }
        };
        entries.push(Entry {
            name: acc.name.clone(),
            password: acc.password.clone(),
            action,
            rows,
        });
    }
    Ok(Plan {
        entries,
        warnings: parsed.warnings.clone(),
        volumes: parsed.volumes.len(),
    })
}

/// The chestcraft volume that stands for a copyparty volume: same name, the only one with
/// the same name in another case, or the one sharing the same folder.
fn matching_volume<'a>(cfg: &'a Config, vol: &copyparty::Volume) -> Option<&'a str> {
    if let Some((name, _)) = cfg.volumes.get_key_value(&vol.url) {
        return Some(name);
    }
    let mut same_case = cfg
        .volumes
        .keys()
        .filter(|k| k.eq_ignore_ascii_case(&vol.url));
    if let (Some(only), None) = (same_case.next(), same_case.next()) {
        return Some(only);
    }
    let folder = Path::new(&vol.path);
    if !folder.is_absolute() {
        return None;
    }
    cfg.volumes
        .values()
        .find(|v| v.path == folder)
        .map(|v| v.name.as_str())
}

fn rows(parsed: &Parsed, cfg: &Config, account: &str, role: Option<&str>) -> Vec<Row> {
    let who = role.map(|r| Principal::from_account(account, r));
    let allowed = |vol: &str| {
        who.as_ref()
            .map(|w| perm::volume_perms(cfg, w, Origin::Local, vol))
            .unwrap_or_default()
    };
    let mut out = Vec::new();
    let mut covered = BTreeSet::new();
    for vol in &parsed.volumes {
        let access = parsed.access(account, vol);
        let target = matching_volume(cfg, vol);
        if let Some(t) = target {
            covered.insert(t);
        }
        let granted = match (target, &who) {
            (Some(t), Some(_)) => Some((t.to_string(), allowed(t))),
            _ => None,
        };
        if access.is_empty() && granted.as_ref().is_none_or(|(_, p)| p.is_empty()) {
            continue;
        }
        out.push(Row {
            copyparty: Some((vol.url.clone(), access)),
            chestcraft: granted,
        });
    }
    if who.is_some() {
        for name in cfg.volumes.keys() {
            let p = allowed(name);
            if !covered.contains(name.as_str()) && !p.is_empty() {
                out.push(Row {
                    copyparty: None,
                    chestcraft: Some((name.clone(), p)),
                });
            }
        }
    }
    out
}

/// An account the import created.
#[derive(Debug)]
pub struct Imported {
    pub name: String,
    pub role: String,
    /// Set when copyparty kept only a hash: the account gets this one-time password.
    pub one_time: Option<String>,
}

/// Create the accounts the plan imports. An account that exists by now stays untouched.
pub fn apply(plan: &Plan, db: &Db, now: i64) -> Result<Vec<Imported>, DbError> {
    let mut out = Vec::new();
    for e in &plan.entries {
        let Action::Import { role } = &e.action else {
            continue;
        };
        let (hash, one_time) = match &e.password {
            Password::Plain(s) => (password::hash(s.expose()), None),
            Password::Hashed => {
                let pw = password::generate();
                (password::hash(&pw), Some(pw))
            }
            Password::Empty => continue,
        };
        let user = User {
            name: e.name.clone(),
            role: role.clone(),
            password_hash: hash,
            must_change_password: true,
            disabled: false,
            created: now,
            last_login: None,
            settings: Default::default(),
            known_remote_ips: vec![],
        };
        if !db.insert_user(&user)? {
            continue;
        }
        db.audit(&AuditEvent {
            time: now,
            actor: "cli".into(),
            ip: String::new(),
            action: "user_imported".into(),
            target: User::key(&e.name),
            detail: format!("from copyparty, role {role}"),
        })?;
        out.push(Imported {
            name: e.name.clone(),
            role: role.clone(),
            one_time,
        });
    }
    Ok(out)
}

fn shown_name(name: &str) -> String {
    if name.is_empty() || name.contains(char::is_whitespace) {
        format!("\"{name}\"")
    } else {
        name.to_string()
    }
}

/// What the plan does, for the terminal. Contains no passwords.
pub fn report(plan: &Plan, source: &Path) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "{}: {} account(s), {} volume(s)",
        source.display(),
        plan.entries.len(),
        plan.volumes
    );
    for w in &plan.warnings {
        let _ = writeln!(s, "warning: {w}");
    }
    let cp_width = plan
        .entries
        .iter()
        .flat_map(|e| &e.rows)
        .filter_map(|r| r.copyparty.as_ref().map(|(u, _)| u.len() + 1))
        .max()
        .unwrap_or(0)
        .max("copyparty".len());
    let cc_width = plan
        .entries
        .iter()
        .flat_map(|e| &e.rows)
        .filter_map(|r| r.chestcraft.as_ref().map(|(v, _)| v.len()))
        .max()
        .unwrap_or(0)
        .max("(no volume)".len());
    for e in &plan.entries {
        s.push('\n');
        let name = shown_name(&e.name);
        let role = match &e.action {
            Action::Import { role } => {
                let how = if e.password == Password::Hashed {
                    "copyparty keeps only a hash, so it gets a one-time password"
                } else {
                    "keeps its copyparty password until it picks a new one at the first sign-in"
                };
                let _ = writeln!(s, "{name} → {role}: import; {how}");
                true
            }
            Action::NeedsRole => {
                let _ = writeln!(s, "{name}: needs a role (--role {name}=<role>) or --skip");
                false
            }
            Action::Skip(why) => {
                let why: Vec<String> = why
                    .iter()
                    .map(|w| match w {
                        Skip::NoPassword => "no password".to_string(),
                        Skip::BadName => {
                            "not a valid account name (1–32 of a–z, A–Z, 0–9, . _ -)".to_string()
                        }
                        Skip::Duplicate(first) => {
                            format!("same name as {first} (names ignore case)")
                        }
                        Skip::Requested => "--skip".to_string(),
                        Skip::Exists => "an account with this name exists already".to_string(),
                    })
                    .collect();
                let _ = writeln!(s, "{name}: skipped: {}", why.join("; "));
                continue;
            }
        };
        if e.rows.is_empty() {
            let _ = writeln!(s, "    no access to any volume");
            continue;
        }
        let header = format!(
            "    {:<cp_width$}        {}",
            "copyparty",
            if role { "chestcraft" } else { "" }
        );
        let _ = writeln!(s, "{}", header.trim_end());
        for r in &e.rows {
            let (url, cp) = match &r.copyparty {
                Some((u, a)) => (format!("/{u}"), a.letters()),
                None => ("-".to_string(), String::new()),
            };
            let cp = if cp.is_empty() { "-".into() } else { cp };
            if !role {
                let _ = writeln!(s, "    {url:<cp_width$} {cp}");
                continue;
            }
            let (vol, cc) = match &r.chestcraft {
                Some((v, p)) if p.is_empty() => (v.clone(), "-".to_string()),
                Some((v, p)) => (v.clone(), p.letters()),
                None => ("(no volume)".to_string(), String::new()),
            };
            let mark = if r.differs() { "  differs" } else { "" };
            let line = format!("    {url:<cp_width$} {cp:<6} {vol:<cc_width$} {cc:<5}{mark}");
            let _ = writeln!(s, "{}", line.trim_end());
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONF: &str = "\
[accounts]
  kim: correct horse battery
  ole: kurz
  jana: geheim
 create volumes:
[/]
  .
  accs:
    A: kim
[/Filme]
  /srv/filme
  accs:
    A: kim
    r: jana
[/Austausch]
  /srv/austausch
  accs:
    r: *
    A: kim
[/Fotos/alt]
  /srv/fotos
  accs:
    rw: jana
";

    fn cfg() -> Config {
        Config::from_toml_str(
            r#"
            [server]
            public_url = "https://files.example.org"
            trusted_proxies = ["192.0.2.10"]
            remote_proxies = ["192.0.2.10"]

            [volumes.Filme]
            path = "/srv/filme"

            [volumes.austausch]
            path = "/srv/austausch"
            public = true

            [volumes.Bilder]
            path = "/srv/fotos"

            [roles.kino]
            Filme = "r"
            Bilder = "rw"
            "#,
        )
        .unwrap()
    }

    fn db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("state.redb")).unwrap();
        (dir, db)
    }

    fn choices(roles: &[(&str, &str)], skip: &[&str]) -> Choices {
        Choices {
            roles: roles
                .iter()
                .map(|(n, r)| (n.to_string(), r.to_string()))
                .collect(),
            skip: skip.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn plan_actions_and_comparison() {
        let parsed = copyparty::parse_str(CONF).unwrap();
        let (_d, db) = db();
        let plan = plan(
            &parsed,
            &cfg(),
            &db,
            &choices(&[("Kim", "admin"), ("jana", "kino")], &[]),
        )
        .unwrap();
        let actions: Vec<_> = plan.entries.iter().map(|e| e.action.clone()).collect();
        assert_eq!(
            actions,
            [
                Action::Import {
                    role: "admin".into()
                },
                Action::NeedsRole,
                Action::Import {
                    role: "kino".into()
                },
                Action::Skip(vec![Skip::NoPassword, Skip::BadName]),
            ]
        );
        assert_eq!(plan.needs_role(), ["ole"]);

        // jana: Filme by name, Austausch in another case (public: everyone reads),
        // Fotos/alt by its folder.
        let jana = &plan.entries[2].rows;
        let view: Vec<(String, String, String, bool)> = jana
            .iter()
            .map(|r| {
                (
                    r.copyparty
                        .as_ref()
                        .map(|(u, _)| u.clone())
                        .unwrap_or_default(),
                    r.copyparty
                        .as_ref()
                        .map(|(_, a)| a.letters())
                        .unwrap_or_default(),
                    r.chestcraft
                        .as_ref()
                        .map(|(v, p)| format!("{v} {}", p.letters()))
                        .unwrap_or_default(),
                    r.differs(),
                )
            })
            .collect();
        assert_eq!(
            view,
            [
                ("Filme".into(), "r".into(), "Filme r".into(), false),
                ("Austausch".into(), "r".into(), "austausch r".into(), false),
                ("Fotos/alt".into(), "rw".into(), "Bilder rw".into(), false),
            ]
        );
        // kim: the web root has no chestcraft volume; Bilder (copyparty /Fotos/alt) is new
        // for kim.
        let kim = &plan.entries[0].rows;
        assert_eq!(kim[0].copyparty.as_ref().unwrap().0, "");
        assert_eq!(kim[0].chestcraft, None);
        assert!(kim[0].differs());
        let bilder = &kim[3];
        assert!(bilder.copyparty.as_ref().unwrap().1.is_empty());
        assert_eq!(
            bilder.chestcraft,
            Some(("Bilder".to_string(), Perms::all()))
        );
        assert!(bilder.differs());
        assert_eq!(kim.len(), 4);

        // A role volume with no copyparty counterpart shows up on its own.
        let mut cfg2 = cfg();
        let extra = cfg2.volumes["Filme"].clone();
        cfg2.volumes.insert(
            "Neu".into(),
            crate::config::VolumeConfig {
                name: "Neu".into(),
                path: "/srv/neu".into(),
                ..extra
            },
        );
        let plan2 = super::plan(&parsed, &cfg2, &db, &choices(&[("kim", "admin")], &[])).unwrap();
        let neu = plan2.entries[0].rows.last().unwrap();
        assert_eq!(neu.copyparty, None);
        assert_eq!(neu.chestcraft.as_ref().unwrap().0, "Neu");

        let text = report(&plan, Path::new("copyparty.conf"));
        assert!(text.contains("kim → admin: import"), "{text}");
        assert!(text.contains("ole: needs a role"), "{text}");
        assert!(
            text.contains("\"create volumes\": skipped: no password;"),
            "{text}"
        );
        for pw in ["correct horse battery", "kurz", "geheim"] {
            assert!(!text.contains(pw), "{text}");
        }
    }

    #[test]
    fn choices_are_checked() {
        let parsed = copyparty::parse_str(CONF).unwrap();
        let (_d, db) = db();
        let cfg = cfg();
        let run = |c: Choices| plan(&parsed, &cfg, &db, &c);
        assert!(matches!(
            run(choices(&[("nobody", "kino")], &[])),
            Err(PlanError::UnknownAccount(_))
        ));
        assert!(matches!(
            run(choices(&[("ole", "chef")], &[])),
            Err(PlanError::UnknownRole { .. })
        ));
        assert!(matches!(
            run(choices(&[("ole", "kino")], &["OLE"])),
            Err(PlanError::Twice(_))
        ));
        let p = run(choices(&[], &["ole"])).unwrap();
        assert_eq!(p.entries[1].action, Action::Skip(vec![Skip::Requested]));
    }

    #[test]
    fn import_keeps_the_password_and_never_overwrites() {
        let parsed = copyparty::parse_str(CONF).unwrap();
        let (_d, db) = db();
        let cfg = cfg();
        let c = choices(&[("kim", "admin"), ("jana", "kino")], &["ole"]);
        let p = plan(&parsed, &cfg, &db, &c).unwrap();
        let done = apply(&p, &db, 1_000).unwrap();
        let names: Vec<_> = done.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["kim", "jana"]);
        assert!(done.iter().all(|i| i.one_time.is_none()));

        let kim = db.user("KIM").unwrap().unwrap();
        assert_eq!(kim.role, "admin");
        assert!(kim.must_change_password);
        assert!(!kim.disabled);
        assert!(password::verify(
            "correct horse battery",
            &kim.password_hash
        ));
        assert!(db.user("ole").unwrap().is_none());
        assert!(db.user("create volumes").unwrap().is_none());
        let audit = db.audit_since(0).unwrap();
        assert_eq!(audit.len(), 2);
        assert_eq!(audit[0].action, "user_imported");
        assert_eq!(audit[0].target, "kim");

        // Running again changes nothing, even after kim picked a new password.
        db.update_user("kim", |u| {
            u.password_hash = password::hash("ein neues Passwort");
            u.must_change_password = false;
        })
        .unwrap();
        let p = plan(&parsed, &cfg, &db, &c).unwrap();
        assert_eq!(p.entries[0].action, Action::Skip(vec![Skip::Exists]));
        assert!(apply(&p, &db, 2_000).unwrap().is_empty());
        let kim = db.user("kim").unwrap().unwrap();
        assert!(password::verify("ein neues Passwort", &kim.password_hash));
        assert!(!kim.must_change_password);
    }

    #[test]
    fn duplicates_and_hashes() {
        let hash = format!("+{}", "x".repeat(32));
        let text = format!(
            "[global]\n  ah-alg: argon2\n[accounts]\n  kim: {hash}\n  Kim: anderes\n[/a]\n  /srv/a\n  accs:\n    r: kim\n"
        );
        let parsed = copyparty::parse_str(&text).unwrap();
        let (_d, db) = db();
        let p = plan(&parsed, &cfg(), &db, &choices(&[("kim", "kino")], &[])).unwrap();
        assert_eq!(
            p.entries[1].action,
            Action::Skip(vec![Skip::Duplicate("kim".into())])
        );
        let done = apply(&p, &db, 1).unwrap();
        let pw = done[0].one_time.clone().unwrap();
        let kim = db.user("kim").unwrap().unwrap();
        assert!(password::verify(&pw, &kim.password_hash));
        assert!(kim.must_change_password);
    }

    #[test]
    fn insert_user_leaves_existing_accounts_alone() {
        let (_d, db) = db();
        let mut u = User {
            name: "Kim".into(),
            role: "kino".into(),
            password_hash: password::hash("erstes Passwort"),
            must_change_password: false,
            disabled: false,
            created: 1,
            last_login: None,
            settings: Default::default(),
            known_remote_ips: vec![],
        };
        assert!(db.insert_user(&u).unwrap());
        u.name = "kim".into();
        u.role = "admin".into();
        assert!(!db.insert_user(&u).unwrap());
        assert_eq!(db.user("kim").unwrap().unwrap().role, "kino");
    }
}
