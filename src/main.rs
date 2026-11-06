use chestcraft::config::{self, Config};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Set by deploy/release.sh from `git describe`; falls back to the crate version.
const VERSION: &str = match option_env!("CHESTCRAFT_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

#[derive(Parser)]
#[command(name = "chestcraft", version = VERSION, about = "File sharing for a home NAS")]
struct Cli {
    /// Configuration file
    #[arg(short, long, global = true, default_value = config::DEFAULT_CONFIG_PATH)]
    config: PathBuf,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the web server
    Serve,
    /// Validate the configuration and the machine it points to
    Check {
        /// Only validate the file; skip the checks against this machine
        #[arg(long)]
        offline: bool,
    },
    /// Read SMART data for the dashboard (runs as root from chestcraft-collect.timer)
    Collect,
    /// Manage accounts (run as the service user while the service is stopped)
    #[command(subcommand)]
    User(UserCmd),
}

#[derive(Subcommand)]
enum UserCmd {
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

#[derive(Subcommand)]
enum AppPasswordCmd {
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

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Cmd::Check { offline } => check(&cli.config, offline),
        Cmd::Serve => serve(&cli.config),
        Cmd::User(cmd) => user(&cli.config, cmd),
        Cmd::Collect => {
            eprintln!("not implemented yet");
            ExitCode::from(2)
        }
    }
}

fn init_logging() {
    use tracing_subscriber::EnvFilter;
    let filter =
        EnvFilter::try_from_env("CHESTCRAFT_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let ansi = std::io::IsTerminal::is_terminal(&std::io::stdout());
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_ansi(ansi);
    // journald stamps every line itself.
    if std::env::var_os("JOURNAL_STREAM").is_some() {
        builder.without_time().init();
    } else {
        builder.init();
    }
}

fn serve(path: &Path) -> ExitCode {
    init_logging();
    let cfg = match Config::load(path) {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::error!("{}:\n{e}", path.display());
            return ExitCode::FAILURE;
        }
    };
    for w in config::check_environment(&cfg).warnings {
        tracing::warn!("{w}");
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!("can't start the async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(chestcraft::web::serve(cfg, path.parent())) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e:#}");
            ExitCode::FAILURE
        }
    }
}

fn check(path: &Path, offline: bool) -> ExitCode {
    let cfg = match Config::load(path) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("{}:\n{e}", path.display());
            return ExitCode::FAILURE;
        }
    };
    if !offline {
        let report = config::check_environment(&cfg);
        for w in &report.warnings {
            eprintln!("warning: {w}");
        }
        for e in &report.errors {
            eprintln!("error: {e}");
        }
        if !report.errors.is_empty() {
            return ExitCode::FAILURE;
        }
    }
    println!(
        "{}: OK ({} volumes, {} roles)",
        path.display(),
        cfg.volumes.len(),
        cfg.roles.len()
    );
    ExitCode::SUCCESS
}

fn valid_username(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && !name.starts_with(['.', '-'])
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn user(path: &Path, cmd: UserCmd) -> ExitCode {
    if rustix::process::geteuid().is_root() {
        eprintln!(
            "Run this as the service user (e.g. `sudo -u chestcraft chestcraft user ...`), not as root:"
        );
        eprintln!("a database file created by root can't be opened by the service.");
        return ExitCode::FAILURE;
    }
    let cfg = match Config::load(path) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("{}:\n{e}", path.display());
            return ExitCode::FAILURE;
        }
    };
    let db = match chestcraft::web::open_db(&cfg) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let role_ok =
        |role: &str| role == chestcraft::auth::perm::ADMIN_ROLE || cfg.roles.contains_key(role);
    let fail = |msg: String| {
        eprintln!("{msg}");
        ExitCode::FAILURE
    };
    let now = jiff::Timestamp::now().as_second();
    let result: Result<ExitCode, chestcraft::db::DbError> = (|| {
        Ok(match cmd {
            UserCmd::Add { name, role } => {
                if !valid_username(&name) {
                    return Ok(fail(format!(
                        "\"{name}\": use 1–32 of a–z, A–Z, 0–9, . _ -"
                    )));
                }
                if !role_ok(&role) {
                    return Ok(fail(format!(
                        "unknown role \"{role}\" (admin or one from [roles])"
                    )));
                }
                if db.user(&name)?.is_some() {
                    return Ok(fail(format!("an account named \"{name}\" exists already")));
                }
                let pw = chestcraft::auth::password::generate();
                db.put_user(&chestcraft::db::User {
                    name: name.clone(),
                    role,
                    password_hash: chestcraft::auth::password::hash(&pw),
                    must_change_password: true,
                    disabled: false,
                    created: now,
                    last_login: None,
                    settings: Default::default(),
                    known_remote_ips: vec![],
                })?;
                println!("Created {name}. One-time password (changed at first sign-in): {pw}");
                ExitCode::SUCCESS
            }
            UserCmd::Passwd { name } => match db.user(&name)? {
                Some(mut u) => {
                    let pw = chestcraft::auth::password::generate();
                    u.password_hash = chestcraft::auth::password::hash(&pw);
                    u.must_change_password = true;
                    db.put_user(&u)?;
                    db.delete_user_sessions(&chestcraft::db::User::key(&u.name), None)?;
                    println!("New one-time password for {}: {pw}", u.name);
                    ExitCode::SUCCESS
                }
                None => fail(format!("no account named \"{name}\"")),
            },
            UserCmd::Role { name, role } => match db.user(&name)? {
                Some(_) if !role_ok(&role) => fail(format!("unknown role \"{role}\"")),
                Some(mut u) => {
                    u.role = role;
                    db.put_user(&u)?;
                    println!("{} now has role {}", u.name, u.role);
                    ExitCode::SUCCESS
                }
                None => fail(format!("no account named \"{name}\"")),
            },
            UserCmd::Disable { name } | UserCmd::Enable { name } if db.user(&name)?.is_none() => {
                fail(format!("no account named \"{name}\""))
            }
            UserCmd::Disable { name } => {
                let mut u = db.user(&name)?.expect("checked above");
                u.disabled = true;
                db.put_user(&u)?;
                let ended = db.delete_user_sessions(&chestcraft::db::User::key(&u.name), None)?;
                println!("{} is disabled; {ended} session(s) ended", u.name);
                ExitCode::SUCCESS
            }
            UserCmd::Enable { name } => {
                let mut u = db.user(&name)?.expect("checked above");
                u.disabled = false;
                db.put_user(&u)?;
                println!("{} is enabled", u.name);
                ExitCode::SUCCESS
            }
            UserCmd::List => {
                let mut users = db.users()?;
                users.sort_by_key(|u| u.name.to_lowercase());
                for u in users {
                    let mut flags = Vec::new();
                    if u.disabled {
                        flags.push("disabled");
                    }
                    if u.must_change_password {
                        flags.push("must change password");
                    }
                    if !role_ok(&u.role) {
                        flags.push("role missing from config");
                    }
                    println!("{:<20} {:<12} {}", u.name, u.role, flags.join(", "));
                }
                ExitCode::SUCCESS
            }
            UserCmd::AppPassword(cmd) => app_password(&db, cmd, &cfg.server.timezone, now)?,
        })
    })();
    result.unwrap_or_else(|e| fail(e.to_string()))
}

fn app_password(
    db: &chestcraft::db::Db,
    cmd: AppPasswordCmd,
    tz: &jiff::tz::TimeZone,
    now: i64,
) -> Result<ExitCode, chestcraft::db::DbError> {
    use chestcraft::auth::apppass::{self, CreateError};
    let fail = |msg: String| {
        eprintln!("{msg}");
        ExitCode::FAILURE
    };
    let date = |t: i64| {
        jiff::Timestamp::from_second(t)
            .map(|ts| {
                ts.to_zoned(tz.clone())
                    .strftime("%Y-%m-%d %H:%M")
                    .to_string()
            })
            .unwrap_or_default()
    };
    Ok(match cmd {
        AppPasswordCmd::Add { name, label } => match apppass::create(db, &name, &label, now) {
            Ok((secret, rec)) => {
                let user = db.user(&name)?.map(|u| u.name).unwrap_or(name);
                println!("App password \"{}\" (id {}) for {user}:", rec.label, rec.id);
                println!("{secret}");
                println!("It is shown only now. Use it with the user name {user} for WebDAV.");
                ExitCode::SUCCESS
            }
            Err(CreateError::Db(e)) => return Err(e),
            Err(CreateError::NoUser) => fail(format!("no account named \"{name}\"")),
            Err(e) => fail(e.to_string()),
        },
        AppPasswordCmd::List { name } => match db.user(&name)? {
            Some(u) => {
                let mut all = db.app_passwords_of(&chestcraft::db::User::key(&u.name))?;
                all.sort_by_key(|(_, r)| r.created);
                if all.is_empty() {
                    println!("{} has no app passwords", u.name);
                }
                for (_, r) in all {
                    let used = match (r.last_used, &r.last_ip) {
                        (Some(t), Some(ip)) => format!("last used {} from {ip}", date(t)),
                        (Some(t), None) => format!("last used {}", date(t)),
                        _ => "never used".to_string(),
                    };
                    println!(
                        "{:<12} {:<30} created {}, {used}",
                        r.id,
                        r.label,
                        date(r.created)
                    );
                }
                ExitCode::SUCCESS
            }
            None => fail(format!("no account named \"{name}\"")),
        },
        AppPasswordCmd::Revoke { name, which } => match db.user(&name)? {
            Some(u) => match apppass::revoke(db, &chestcraft::db::User::key(&u.name), &which)? {
                Some(r) => {
                    println!("Revoked \"{}\" (id {}) of {}", r.label, r.id, u.name);
                    ExitCode::SUCCESS
                }
                None => fail(format!("{} has no app password \"{which}\"", u.name)),
            },
            None => fail(format!("no account named \"{name}\"")),
        },
    })
}
