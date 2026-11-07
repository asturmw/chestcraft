//! CLI commands that change the state: accounts, agents, status API tokens, the backup.
//!
//! They run in one of two places. While the server runs, it holds the database (redb
//! allows one process), so `chestcraft user …` sends the parsed command as JSON to the
//! server's admin socket (`paths.admin_socket`, mode 600; [`serve_socket`] checks the
//! caller's uid) and prints what comes back. Otherwise the CLI opens the database itself
//! and runs the same [`run`]. Output is collected in [`Out`] either way.

use crate::auth::accounts;
use crate::config::{self, Config};
use crate::db::{AuditEvent, Db, DbError, User};
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::path::Path;

#[derive(Subcommand, Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum AdminCmd {
    /// Manage accounts
    #[command(subcommand)]
    User(UserCmd),
    /// Manage agent accounts for the MCP server and their tokens
    #[command(subcommand)]
    Agent(AgentCmd),
    /// Manage read-only tokens for the status API, /api/v1/status (the admin can also do
    /// this on the volumes page)
    #[command(subcommand, name = "api-token")]
    ApiToken(ApiTokenCmd),
    /// Copy the state to paths.backup_dir now (it also happens every night)
    Backup,
}

#[derive(Subcommand, Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum UserCmd {
    /// Create an account; prints a one-time password to change at first sign-in
    Add {
        name: String,
        /// "admin" or a role from the config
        #[arg(long)]
        role: String,
    },
    /// Give an account a new one-time password
    Passwd { name: String },
    /// Change an account's role
    Role { name: String, role: String },
    /// Block an account and end its sessions
    Disable { name: String },
    /// Unblock an account
    Enable { name: String },
    /// List accounts
    List,
    /// Manage an account's WebDAV app passwords (one per device)
    #[command(subcommand, name = "app-password")]
    AppPassword(AppPasswordCmd),
}

#[derive(Subcommand, Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum AppPasswordCmd {
    /// Create an app password; prints the secret once
    Add {
        name: String,
        /// Which device it is for, e.g. "laptop rclone"
        label: String,
    },
    /// List an account's app passwords
    List { name: String },
    /// Revoke an app password by its id or label
    Revoke { name: String, which: String },
}

#[derive(Subcommand, Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum AgentCmd {
    /// Create an agent account: a role from [roles] (never admin), no password
    Add {
        name: String,
        #[arg(long)]
        role: String,
    },
    /// List agents and their tokens
    List,
    /// Change an agent's role
    Role { name: String, role: String },
    /// Block an agent: its tokens stop working and its shares pause
    Disable { name: String },
    /// Unblock an agent
    Enable { name: String },
    /// Create a token for one host the agent runs on; prints it once
    Token {
        name: String,
        /// Where it is used, e.g. "workstation claude"
        label: String,
        /// Stop working after this long, e.g. 90d or 12h (default: never)
        #[arg(long, value_name = "AGE")]
        expires: Option<String>,
    },
    /// Revoke a token by its id or label
    Revoke { name: String, which: String },
}

#[derive(Subcommand, Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum ApiTokenCmd {
    /// Create a token; prints it once
    Add {
        /// What it is for, e.g. "watchdog"
        label: String,
    },
    /// List tokens
    List,
    /// Revoke a token by its id or label
    Revoke { which: String },
}

impl AdminCmd {
    /// For the log: which command, without its arguments.
    pub fn label(&self) -> &'static str {
        match self {
            AdminCmd::User(c) => match c {
                UserCmd::Add { .. } => "user add",
                UserCmd::Passwd { .. } => "user passwd",
                UserCmd::Role { .. } => "user role",
                UserCmd::Disable { .. } => "user disable",
                UserCmd::Enable { .. } => "user enable",
                UserCmd::List => "user list",
                UserCmd::AppPassword(AppPasswordCmd::Add { .. }) => "user app-password add",
                UserCmd::AppPassword(AppPasswordCmd::List { .. }) => "user app-password list",
                UserCmd::AppPassword(AppPasswordCmd::Revoke { .. }) => "user app-password revoke",
            },
            AdminCmd::Agent(c) => match c {
                AgentCmd::Add { .. } => "agent add",
                AgentCmd::List => "agent list",
                AgentCmd::Role { .. } => "agent role",
                AgentCmd::Disable { .. } => "agent disable",
                AgentCmd::Enable { .. } => "agent enable",
                AgentCmd::Token { .. } => "agent token",
                AgentCmd::Revoke { .. } => "agent revoke",
            },
            AdminCmd::ApiToken(c) => match c {
                ApiTokenCmd::Add { .. } => "api-token add",
                ApiTokenCmd::List => "api-token list",
                ApiTokenCmd::Revoke { .. } => "api-token revoke",
            },
            AdminCmd::Backup => "backup",
        }
    }
}

/// What a command printed and its exit code.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Out {
    pub stdout: String,
    pub stderr: String,
    pub code: u8,
}

impl Out {
    fn say(&mut self, line: impl AsRef<str>) {
        self.stdout.push_str(line.as_ref());
        self.stdout.push('\n');
    }

    /// Print an error and mark the command failed.
    fn fail(&mut self, line: impl AsRef<str>) {
        self.stderr.push_str(line.as_ref());
        self.stderr.push('\n');
        self.code = 1;
    }
}

/// Outcome of [`send`].
#[derive(Debug)]
pub enum Remote {
    /// No server is listening: run the command here.
    NoServer,
    /// The server ran it.
    Done(Out),
}

/// Send `cmd` to the server's admin socket at `path`. Blocking.
pub fn send(path: &Path, cmd: &AdminCmd) -> Result<Remote, String> {
    use std::io::{ErrorKind, Read, Write};
    use std::os::unix::net::UnixStream;
    let mut stream = match UnixStream::connect(path) {
        Ok(s) => s,
        // No socket, or one left behind by a server that is gone.
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused) => {
            return Ok(Remote::NoServer);
        }
        Err(e) if e.kind() == ErrorKind::PermissionDenied => {
            return Err(format!(
                "{}: permission denied; run this as the service user or as root",
                path.display()
            ));
        }
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let io = |e: std::io::Error| format!("talking to the server ({}): {e}", path.display());
    stream
        .set_write_timeout(Some(std::time::Duration::from_secs(10)))
        .map_err(io)?;
    // A backup of a large database may take a while.
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(600)))
        .map_err(io)?;
    let req = serde_json::json!({ "version": crate::VERSION, "cmd": cmd });
    let mut line = serde_json::to_vec(&req).map_err(|e| e.to_string())?;
    line.push(b'\n');
    stream.write_all(&line).map_err(io)?;
    let mut answer = String::new();
    stream.read_to_string(&mut answer).map_err(io)?;
    serde_json::from_str(&answer)
        .map(Remote::Done)
        .map_err(|e| format!("the server's answer makes no sense ({e})"))
}

/// What a command runs against.
pub struct Env<'a> {
    pub cfg: &'a Config,
    pub db: &'a Db,
    /// The config file (copied by `backup`).
    pub config_file: Option<&'a Path>,
}

/// Run `cmd`. Blocking (database, password hashing).
pub fn run(cmd: AdminCmd, env: &Env, now: i64) -> Out {
    let mut out = Out::default();
    let result = match cmd {
        AdminCmd::User(c) => user(c, env, now, &mut out),
        AdminCmd::Agent(c) => agent(c, env, now, &mut out).or_else(|e| match e {
            crate::auth::agent::AgentError::Db(e) => Err(e),
            other => {
                out.fail(other.to_string());
                Ok(())
            }
        }),
        AdminCmd::ApiToken(c) => api_token(c, env, now, &mut out),
        AdminCmd::Backup => {
            backup(env, &mut out);
            Ok(())
        }
    };
    if let Err(e) = result {
        out.fail(e.to_string());
    }
    out
}

fn date(tz: &jiff::tz::TimeZone, t: i64) -> String {
    jiff::Timestamp::from_second(t)
        .map(|ts| {
            ts.to_zoned(tz.clone())
                .strftime("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

fn audit(db: &Db, now: i64, action: &str, target: &str, detail: &str) -> Result<(), DbError> {
    db.audit(&AuditEvent {
        time: now,
        actor: "cli".into(),
        ip: String::new(),
        action: action.into(),
        target: target.into(),
        detail: detail.into(),
    })
}

fn backup(env: &Env, out: &mut Out) {
    let paths = &env.cfg.paths;
    let result = match crate::backup::refuse(&paths.state_dir, &paths.backup_dir) {
        Some(why) => Err(why),
        None => crate::backup::run(env.db, &paths.state_dir, &paths.backup_dir, env.config_file),
    };
    let mut status = env.db.backup_status().ok().flatten().unwrap_or_default();
    status.attempt = jiff::Timestamp::now().as_second();
    match result {
        Ok(r) => {
            out.say(format!(
                "Backed up to {}: database {} rows ({} bytes), {} config file(s), {} picture(s) \
                 copied, {} dropped",
                paths.backup_dir.display(),
                r.rows,
                r.bytes,
                r.config_files,
                r.pictures_copied,
                r.pictures_dropped
            ));
            status.last_ok = Some(status.attempt);
            status.error = None;
            status.bytes = r.bytes;
        }
        Err(e) => {
            out.fail(format!("backup failed: {e}"));
            status.error = Some(e);
        }
    }
    if let Err(e) = env.db.put_backup_status(&status) {
        out.fail(format!("can't record the backup outcome: {e}"));
    }
}

fn user(cmd: UserCmd, env: &Env, now: i64, out: &mut Out) -> Result<(), DbError> {
    let (cfg, db) = (env.cfg, env.db);
    let role_ok = |role: &str| accounts::role_exists(cfg, role);
    match cmd {
        UserCmd::Add { name, role } => match accounts::create_person(db, cfg, &name, &role, now) {
            Ok((u, pw)) => {
                audit(db, now, "user_created", &User::key(&u.name), &u.role)?;
                out.say(format!(
                    "Created {}. One-time password (changed at first sign-in): {pw}",
                    u.name
                ));
            }
            Err(e) => return account_error(e, out),
        },
        UserCmd::Passwd { name } => match accounts::reset_password(db, &name) {
            Ok((u, pw)) => {
                audit(db, now, "user_password_reset", &User::key(&u.name), "")?;
                out.say(format!("New one-time password for {}: {pw}", u.name));
            }
            Err(e) => return account_error(e, out),
        },
        UserCmd::Role { name, role } => match accounts::set_role(db, cfg, &name, &role) {
            Ok(u) => {
                audit(db, now, "user_role", &User::key(&u.name), &u.role)?;
                out.say(format!("{} now has role {}", u.name, u.role));
            }
            Err(e) => return account_error(e, out),
        },
        UserCmd::Disable { name } => match accounts::set_disabled(db, &name, true) {
            Ok((u, ended)) => {
                audit(db, now, "user_disabled", &User::key(&u.name), "")?;
                out.say(format!("{} is disabled; {ended} session(s) ended", u.name));
            }
            Err(e) => return account_error(e, out),
        },
        UserCmd::Enable { name } => match accounts::set_disabled(db, &name, false) {
            Ok((u, _)) => {
                audit(db, now, "user_enabled", &User::key(&u.name), "")?;
                out.say(format!("{} is enabled", u.name));
            }
            Err(e) => return account_error(e, out),
        },
        UserCmd::List => {
            let mut users = db.users()?;
            users.sort_by_key(|u| u.name.to_lowercase());
            for u in users {
                let mut flags = Vec::new();
                if u.is_agent() {
                    flags.push("agent");
                }
                if u.disabled {
                    flags.push("disabled");
                }
                if u.must_change_password {
                    flags.push("must change password");
                }
                if !role_ok(&u.role) {
                    flags.push("role missing from config");
                }
                out.say(format!(
                    "{:<20} {:<12} {}",
                    u.name,
                    u.role,
                    flags.join(", ")
                ));
            }
        }
        UserCmd::AppPassword(cmd) => app_password(cmd, env, now, out)?,
    }
    Ok(())
}

/// Database errors go up; the rest is the user's mistake.
fn account_error(e: accounts::AccountError, out: &mut Out) -> Result<(), DbError> {
    match e {
        accounts::AccountError::Db(e) => Err(e),
        other => {
            out.fail(other.to_string());
            Ok(())
        }
    }
}

fn app_password(cmd: AppPasswordCmd, env: &Env, now: i64, out: &mut Out) -> Result<(), DbError> {
    use crate::auth::apppass::{self, CreateError};
    let (db, tz) = (env.db, &env.cfg.server.timezone);
    match cmd {
        AppPasswordCmd::Add { name, label } => match apppass::create(db, &name, &label, now) {
            Ok((secret, rec)) => {
                let user = db.user(&name)?.map(|u| u.name).unwrap_or(name);
                audit(
                    db,
                    now,
                    "app_password_created",
                    &User::key(&user),
                    &format!("{} {}", rec.id, rec.label),
                )?;
                out.say(format!(
                    "App password \"{}\" (id {}) for {user}:",
                    rec.label, rec.id
                ));
                out.say(&secret);
                out.say(format!(
                    "It is shown only now. Use it with the user name {user} for WebDAV."
                ));
            }
            Err(CreateError::Db(e)) => return Err(e),
            Err(CreateError::NoUser) => out.fail(format!("no account named \"{name}\"")),
            Err(e) => out.fail(e.to_string()),
        },
        AppPasswordCmd::List { name } => match db.user(&name)? {
            Some(u) => {
                let mut all = db.app_passwords_of(&User::key(&u.name))?;
                all.sort_by_key(|(_, r)| r.created);
                if all.is_empty() {
                    out.say(format!("{} has no app passwords", u.name));
                }
                for (_, r) in all {
                    let used = match (r.last_used, &r.last_ip) {
                        (Some(t), Some(ip)) => format!("last used {} from {ip}", date(tz, t)),
                        (Some(t), None) => format!("last used {}", date(tz, t)),
                        _ => "never used".to_string(),
                    };
                    out.say(format!(
                        "{:<12} {:<30} created {}, {used}",
                        r.id,
                        r.label,
                        date(tz, r.created)
                    ));
                }
            }
            None => out.fail(format!("no account named \"{name}\"")),
        },
        AppPasswordCmd::Revoke { name, which } => match db.user(&name)? {
            Some(u) => match apppass::revoke(db, &User::key(&u.name), &which)? {
                Some(r) => {
                    audit(
                        db,
                        now,
                        "app_password_revoked",
                        &User::key(&u.name),
                        &format!("{} {}", r.id, r.label),
                    )?;
                    out.say(format!(
                        "Revoked \"{}\" (id {}) of {}",
                        r.label, r.id, u.name
                    ));
                }
                None => out.fail(format!("{} has no app password \"{which}\"", u.name)),
            },
            None => out.fail(format!("no account named \"{name}\"")),
        },
    }
    Ok(())
}

fn api_token(cmd: ApiTokenCmd, env: &Env, now: i64, out: &mut Out) -> Result<(), DbError> {
    use crate::auth::apitoken::{self, CreateError};
    let (db, tz) = (env.db, &env.cfg.server.timezone);
    match cmd {
        ApiTokenCmd::Add { label } => match apitoken::create(db, &label, "cli", now) {
            Ok((token, rec)) => {
                audit(db, now, "api_token_created", &rec.label, &rec.id)?;
                out.say(format!(
                    "Status API token \"{}\" (id {}):",
                    rec.label, rec.id
                ));
                out.say(&token);
                out.say(
                    "It is shown only now. Send it as \"Authorization: Bearer <token>\" to \
                     /api/v1/status, from the LAN or Tailscale.",
                );
            }
            Err(CreateError::Db(e)) => return Err(e),
            Err(e) => out.fail(e.to_string()),
        },
        ApiTokenCmd::List => {
            let all = db.api_tokens()?;
            if all.is_empty() {
                out.say("No status API tokens");
            }
            for (_, r) in all {
                let used = match (r.last_used, &r.last_ip) {
                    (Some(t), Some(ip)) => format!("last used {} from {ip}", date(tz, t)),
                    (Some(t), None) => format!("last used {}", date(tz, t)),
                    _ => "never used".to_string(),
                };
                out.say(format!(
                    "{:<12} {:<30} created {} by {}, {used}",
                    r.id,
                    r.label,
                    date(tz, r.created),
                    r.created_by
                ));
            }
        }
        ApiTokenCmd::Revoke { which } => match apitoken::revoke(db, &which)? {
            Some(r) => {
                audit(db, now, "api_token_revoked", &r.label, &r.id)?;
                out.say(format!("Revoked \"{}\" (id {})", r.label, r.id));
            }
            None => out.fail(format!("no token \"{which}\"")),
        },
    }
    Ok(())
}

fn agent(
    cmd: AgentCmd,
    env: &Env,
    now: i64,
    out: &mut Out,
) -> Result<(), crate::auth::agent::AgentError> {
    use crate::auth::agent;
    let (cfg, db, tz) = (env.cfg, env.db, &env.cfg.server.timezone);
    match cmd {
        AgentCmd::Add { name, role } => {
            let a = agent::create(db, cfg, &name, &role, now)?;
            audit(db, now, "agent_created", &User::key(&a.name), &a.role)?;
            out.say(format!("Created agent {} with role {}.", a.name, a.role));
            out.say(format!(
                "Give it a token per host: chestcraft agent token {} LABEL [--expires 90d]",
                a.name
            ));
            if cfg.mcp.listen.is_none() {
                out.say("Note: [mcp] listen isn't set, so the MCP server is off.");
            }
        }
        AgentCmd::List => {
            let mut agents: Vec<User> = db.users()?.into_iter().filter(User::is_agent).collect();
            agents.sort_by_key(|u| u.name.to_lowercase());
            if agents.is_empty() {
                out.say("No agents");
            }
            let tokens = db.agent_tokens()?;
            for a in agents {
                let mut flags = Vec::new();
                if a.disabled {
                    flags.push("disabled".to_string());
                }
                if agent::check_role(cfg, &a.role).is_err() {
                    flags.push("role missing from config".into());
                }
                if cfg.mcp.health_roles.contains(&a.role) {
                    flags.push("health tools".into());
                }
                out.say(format!(
                    "{:<20} {:<12} {}",
                    a.name,
                    a.role,
                    flags.join(", ")
                ));
                let key = User::key(&a.name);
                for (_, t) in tokens.iter().filter(|(_, t)| t.agent == key) {
                    let used = match (t.last_used, &t.last_ip) {
                        (Some(u), Some(ip)) => format!("last used {} from {ip}", date(tz, u)),
                        (Some(u), None) => format!("last used {}", date(tz, u)),
                        _ => "never used".to_string(),
                    };
                    let expiry = match t.expires {
                        Some(e) if e <= now => format!(", expired {}", date(tz, e)),
                        Some(e) => format!(", expires {}", date(tz, e)),
                        None => String::new(),
                    };
                    let mut line = String::new();
                    let _ = write!(
                        line,
                        "    token {:<12} {:<30} created {}{expiry}, {used}",
                        t.id,
                        t.label,
                        date(tz, t.created)
                    );
                    out.say(line);
                }
            }
        }
        AgentCmd::Role { name, role } => {
            let a = agent::get(db, &name)?;
            agent::check_role(cfg, &role)?;
            db.update_user(&a.name, |u| u.role = role.clone())?;
            audit(db, now, "agent_role", &User::key(&a.name), &role)?;
            out.say(format!("{} now has role {role}", a.name));
        }
        AgentCmd::Disable { name } => {
            let a = agent::get(db, &name)?;
            db.update_user(&a.name, |u| u.disabled = true)?;
            audit(db, now, "agent_disabled", &User::key(&a.name), "")?;
            out.say(format!(
                "{} is disabled: its tokens stop working and its shares pause",
                a.name
            ));
        }
        AgentCmd::Enable { name } => {
            let a = agent::get(db, &name)?;
            db.update_user(&a.name, |u| u.disabled = false)?;
            audit(db, now, "agent_enabled", &User::key(&a.name), "")?;
            out.say(format!("{} is enabled", a.name));
        }
        AgentCmd::Token {
            name,
            label,
            expires,
        } => {
            let expires = match expires.as_deref().map(config::parse_age) {
                None => None,
                Some(Ok(secs)) if secs > 0 => Some(now + secs),
                Some(_) => {
                    out.fail("--expires takes an age like 90d or 12h");
                    return Ok(());
                }
            };
            let (token, rec) = agent::create_token(db, &name, &label, expires, "cli", now)?;
            audit(
                db,
                now,
                "agent_token_created",
                &rec.agent,
                &format!("{} {}", rec.id, rec.label),
            )?;
            out.say(format!(
                "MCP token \"{}\" (id {}) for agent {}:",
                rec.label, rec.id, rec.agent
            ));
            out.say(&token);
            out.say("It is shown only now. Send it as \"Authorization: Bearer <token>\".");
            if let Some(e) = rec.expires {
                out.say(format!("It stops working {}.", date(tz, e)));
            }
        }
        AgentCmd::Revoke { name, which } => {
            let a = agent::get(db, &name)?;
            match agent::revoke(db, &User::key(&a.name), &which)? {
                Some(r) => {
                    audit(
                        db,
                        now,
                        "agent_token_revoked",
                        &r.agent,
                        &format!("{} {}", r.id, r.label),
                    )?;
                    out.say(format!(
                        "Revoked \"{}\" (id {}) of {}",
                        r.label, r.id, a.name
                    ));
                }
                None => out.fail(format!("{} has no token \"{which}\"", a.name)),
            }
        }
    }
    Ok(())
}
