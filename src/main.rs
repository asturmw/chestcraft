use chestcraft::config::{self, Config};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use chestcraft::VERSION;

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
    /// Find the Telegram chat for alerts and send it a test message (run as root: reads the
    /// bot token from secrets.toml next to the config)
    TelegramSetup {
        /// Secrets file with telegram_token (default: secrets.toml next to the config)
        #[arg(long)]
        secrets: Option<PathBuf>,
    },
    /// Manage accounts (run as the service user while the service is stopped)
    #[command(subcommand)]
    User(UserCmd),
    /// Manage agent accounts for the MCP server and their tokens (run as the service user
    /// while the service is stopped)
    #[command(subcommand)]
    Agent(AgentCmd),
    /// Manage read-only tokens for the status API, /api/v1/status (run as the service user
    /// while the service is stopped; the admin can also do this on the volumes page)
    #[command(subcommand, name = "api-token")]
    ApiToken(ApiTokenCmd),
    /// Create accounts from a copyparty config file; each keeps its copyparty password until
    /// it picks a new one at the first sign-in. Existing accounts are never changed. Run as
    /// the service user while the service is stopped; the file is only read.
    ImportCopyparty {
        /// copyparty config file
        file: PathBuf,
        /// Role for an account ("admin" or one from [roles]); once per account
        #[arg(long = "role", value_name = "NAME=ROLE", value_parser = parse_role)]
        roles: Vec<(String, String)>,
        /// Leave an account out; once per account
        #[arg(long, value_name = "NAME")]
        skip: Vec<String>,
        /// Only show what would happen
        #[arg(long)]
        dry_run: bool,
    },
}

fn parse_role(s: &str) -> Result<(String, String), String> {
    match s.rsplit_once('=') {
        Some((name, role)) if !name.trim().is_empty() && !role.trim().is_empty() => {
            Ok((name.trim().to_string(), role.trim().to_string()))
        }
        _ => Err("use NAME=ROLE, e.g. --role jana=familie".to_string()),
    }
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

#[derive(Subcommand)]
enum AgentCmd {
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

#[derive(Subcommand)]
enum ApiTokenCmd {
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

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Cmd::Check { offline } => check(&cli.config, offline),
        Cmd::Serve => serve(&cli.config),
        Cmd::User(cmd) => user(&cli.config, cmd),
        Cmd::ApiToken(cmd) => api_token(&cli.config, cmd),
        Cmd::Agent(cmd) => agent(&cli.config, cmd),
        Cmd::Collect => collect(&cli.config),
        Cmd::TelegramSetup { secrets } => telegram_setup(&cli.config, secrets),
        Cmd::ImportCopyparty {
            file,
            roles,
            skip,
            dry_run,
        } => import_copyparty(&cli.config, &file, roles, skip, dry_run),
    }
}

/// State commands must not create files that the service (another user) can't open.
fn refuse_root(what: &str) -> Option<ExitCode> {
    if !rustix::process::geteuid().is_root() {
        return None;
    }
    eprintln!(
        "Run this as the service user (e.g. `sudo -u chestcraft chestcraft {what} ...`), not as root:"
    );
    eprintln!("a database file created by root can't be opened by the service.");
    Some(ExitCode::FAILURE)
}

fn import_copyparty(
    path: &Path,
    file: &Path,
    roles: Vec<(String, String)>,
    skip: Vec<String>,
    dry_run: bool,
) -> ExitCode {
    use chestcraft::import::{self, Action, Choices};
    if let Some(code) = refuse_root("import-copyparty") {
        return code;
    }
    let cfg = match Config::load(path) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("{}:\n{e}", path.display());
            return ExitCode::FAILURE;
        }
    };
    let parsed = match import::copyparty::parse_file(file) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
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
    let plan = match import::plan(&parsed, &cfg, &db, &Choices { roles, skip }) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    print!("{}", import::report(&plan, file));
    println!();
    let missing = plan.needs_role();
    if !missing.is_empty() {
        let mut available = vec![chestcraft::auth::perm::ADMIN_ROLE.to_string()];
        available.extend(cfg.roles.keys().cloned());
        println!(
            "Give each of {} a role with --role NAME=ROLE (roles: {}) or leave it out with --skip NAME.",
            missing.join(", "),
            available.join(", ")
        );
        if dry_run {
            return ExitCode::SUCCESS;
        }
        eprintln!("Nothing imported.");
        return ExitCode::FAILURE;
    }
    let count = plan
        .entries
        .iter()
        .filter(|e| matches!(e.action, Action::Import { .. }))
        .count();
    if dry_run {
        println!("Dry run: {count} account(s) would be imported; nothing changed.");
        return ExitCode::SUCCESS;
    }
    if count == 0 {
        println!("Nothing to import.");
        return ExitCode::SUCCESS;
    }
    let now = jiff::Timestamp::now().as_second();
    let done = match import::apply(&plan, &db, now) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let list: Vec<String> = done
        .iter()
        .map(|i| format!("{} ({})", i.name, i.role))
        .collect();
    println!("Imported {}: {}.", done.len(), list.join(", "));
    for i in done.iter().filter(|i| i.one_time.is_some()) {
        println!(
            "One-time password for {} (changed at first sign-in): {}",
            i.name,
            i.one_time.as_deref().unwrap_or_default()
        );
    }
    println!(
        "They sign in with name + password and must pick a new password (at least {} characters).",
        chestcraft::auth::password::MIN_ACCOUNT_PASSWORD
    );
    println!("WebDAV clients need an app password: chestcraft user app-password add NAME LABEL");
    ExitCode::SUCCESS
}

fn init_logging() {
    use tracing_subscriber::EnvFilter;
    let filter =
        // rmcp logs every MCP request at info; the audit log has those.
        EnvFilter::try_from_env("CHESTCRAFT_LOG").unwrap_or_else(|_| EnvFilter::new("info,rmcp=warn"));
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

fn collect(path: &Path) -> ExitCode {
    if !rustix::process::geteuid().is_root() {
        eprintln!("chestcraft collect reads SMART data and must run as root");
        return ExitCode::FAILURE;
    }
    let cfg = match Config::load(path) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("{}:\n{e}", path.display());
            return ExitCode::FAILURE;
        }
    };
    match chestcraft::health::collect::run(&cfg) {
        Ok(summary) => {
            println!("{summary}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn telegram_setup(path: &Path, secrets: Option<PathBuf>) -> ExitCode {
    use chestcraft::config::secrets::Secrets;
    use chestcraft::notify;
    let cfg = match Config::load(path) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("{}:\n{e}", path.display());
            return ExitCode::FAILURE;
        }
    };
    let secrets_path = secrets.unwrap_or_else(|| {
        path.parent()
            .unwrap_or(Path::new("/"))
            .join(chestcraft::config::secrets::FILE_NAME)
    });
    let token = match Secrets::load(&secrets_path) {
        Ok(Secrets {
            telegram_token: Some(t),
            ..
        }) => t,
        Ok(_) => {
            eprintln!(
                "No telegram_token in {}. Create the bot with @BotFather, then put\n  telegram_token = \"<token>\"\ninto that file (owner root, mode 600) and run this again.",
                secrets_path.display()
            );
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let chats = match rt.block_on(notify::telegram_chats(&token)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Asking Telegram for the bot's chats failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    let configured = cfg.alerts.telegram.as_ref().map(|t| t.chat_id);
    if chats.is_empty() && configured.is_none() {
        eprintln!(
            "No chat has written to the bot yet. Open the bot in Telegram, send it /start, \
             then run this again."
        );
        return ExitCode::FAILURE;
    }
    for (id, desc) in &chats {
        println!("chat {id}: {desc}");
    }
    let target = configured.or_else(|| (chats.len() == 1).then(|| chats[0].0));
    let Some(chat_id) = target else {
        println!(
            "\nPick one and put it into the config:\n  [alerts.telegram]\n  chat_id = <id>\nthen run this again to send a test message."
        );
        return ExitCode::SUCCESS;
    };
    let msg = notify::Message {
        level: notify::Level::Info,
        title: "chestcraft".into(),
        body: "Test message: alerts will arrive in this chat.".into(),
    };
    let result = rt.block_on(notify::Channel::telegram(chat_id, token).send(&msg));
    match result {
        Ok(()) => {
            println!("\nSent a test message to chat {chat_id}.");
            if configured.is_none() {
                println!("Put this into the config:\n  [alerts.telegram]\n  chat_id = {chat_id}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("Sending the test message to chat {chat_id} failed: {e}");
            ExitCode::FAILURE
        }
    }
}

fn user(path: &Path, cmd: UserCmd) -> ExitCode {
    if let Some(code) = refuse_root("user") {
        return code;
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
                if !chestcraft::auth::valid_username(&name) {
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
                    kind: Default::default(),
                })?;
                println!("Created {name}. One-time password (changed at first sign-in): {pw}");
                ExitCode::SUCCESS
            }
            UserCmd::Passwd { name } => match db.user(&name)? {
                Some(u) if u.is_agent() => fail(format!(
                    "{} is an agent: it has no password; give it a token with `chestcraft agent token`",
                    u.name
                )),
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
                Some(u) if u.is_agent() && role == chestcraft::auth::perm::ADMIN_ROLE => fail(
                    format!("{} is an agent; agents can't have the admin role", u.name),
                ),
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
                    println!("{:<20} {:<12} {}", u.name, u.role, flags.join(", "));
                }
                ExitCode::SUCCESS
            }
            UserCmd::AppPassword(cmd) => app_password(&db, cmd, &cfg.server.timezone, now)?,
        })
    })();
    result.unwrap_or_else(|e| fail(e.to_string()))
}

fn api_token(path: &Path, cmd: ApiTokenCmd) -> ExitCode {
    use chestcraft::auth::apitoken::{self, CreateError};
    use chestcraft::db::AuditEvent;
    if let Some(code) = refuse_root("api-token") {
        return code;
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
    let fail = |msg: String| {
        eprintln!("{msg}");
        ExitCode::FAILURE
    };
    let tz = &cfg.server.timezone;
    let date = |t: i64| {
        jiff::Timestamp::from_second(t)
            .map(|ts| {
                ts.to_zoned(tz.clone())
                    .strftime("%Y-%m-%d %H:%M")
                    .to_string()
            })
            .unwrap_or_default()
    };
    let now = jiff::Timestamp::now().as_second();
    let audit = |action: &str, rec: &chestcraft::db::ApiToken| {
        db.audit(&AuditEvent {
            time: now,
            actor: "cli".into(),
            ip: String::new(),
            action: action.into(),
            target: rec.label.clone(),
            detail: rec.id.clone(),
        })
    };
    let result: Result<ExitCode, chestcraft::db::DbError> = (|| {
        Ok(match cmd {
            ApiTokenCmd::Add { label } => match apitoken::create(&db, &label, "cli", now) {
                Ok((token, rec)) => {
                    audit("api_token_created", &rec)?;
                    println!("Status API token \"{}\" (id {}):", rec.label, rec.id);
                    println!("{token}");
                    println!(
                        "It is shown only now. Send it as \"Authorization: Bearer <token>\" to \
                         /api/v1/status, from the LAN or Tailscale."
                    );
                    ExitCode::SUCCESS
                }
                Err(CreateError::Db(e)) => return Err(e),
                Err(e) => fail(e.to_string()),
            },
            ApiTokenCmd::List => {
                let all = db.api_tokens()?;
                if all.is_empty() {
                    println!("No status API tokens");
                }
                for (_, r) in all {
                    let used = match (r.last_used, &r.last_ip) {
                        (Some(t), Some(ip)) => format!("last used {} from {ip}", date(t)),
                        (Some(t), None) => format!("last used {}", date(t)),
                        _ => "never used".to_string(),
                    };
                    println!(
                        "{:<12} {:<30} created {} by {}, {used}",
                        r.id,
                        r.label,
                        date(r.created),
                        r.created_by
                    );
                }
                ExitCode::SUCCESS
            }
            ApiTokenCmd::Revoke { which } => match apitoken::revoke(&db, &which)? {
                Some(r) => {
                    audit("api_token_revoked", &r)?;
                    println!("Revoked \"{}\" (id {})", r.label, r.id);
                    ExitCode::SUCCESS
                }
                None => fail(format!("no token \"{which}\"")),
            },
        })
    })();
    result.unwrap_or_else(|e| fail(e.to_string()))
}

fn agent(path: &Path, cmd: AgentCmd) -> ExitCode {
    use chestcraft::auth::agent::{self, AgentError};
    use chestcraft::db::{AuditEvent, User};
    if let Some(code) = refuse_root("agent") {
        return code;
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
    let fail = |msg: String| {
        eprintln!("{msg}");
        ExitCode::FAILURE
    };
    let tz = &cfg.server.timezone;
    let date = |t: i64| {
        jiff::Timestamp::from_second(t)
            .map(|ts| {
                ts.to_zoned(tz.clone())
                    .strftime("%Y-%m-%d %H:%M")
                    .to_string()
            })
            .unwrap_or_default()
    };
    let now = jiff::Timestamp::now().as_second();
    let audit = |action: &str, target: &str, detail: &str| {
        db.audit(&AuditEvent {
            time: now,
            actor: "cli".into(),
            ip: String::new(),
            action: action.into(),
            target: target.into(),
            detail: detail.into(),
        })
    };
    let result: Result<ExitCode, AgentError> = (|| {
        Ok(match cmd {
            AgentCmd::Add { name, role } => {
                let a = agent::create(&db, &cfg, &name, &role, now)?;
                audit("agent_created", &User::key(&a.name), &a.role)?;
                println!("Created agent {} with role {}.", a.name, a.role);
                println!(
                    "Give it a token per host: chestcraft agent token {} LABEL [--expires 90d]",
                    a.name
                );
                if cfg.mcp.listen.is_none() {
                    println!("Note: [mcp] listen isn't set, so the MCP server is off.");
                }
                ExitCode::SUCCESS
            }
            AgentCmd::List => {
                let mut agents: Vec<User> =
                    db.users()?.into_iter().filter(User::is_agent).collect();
                agents.sort_by_key(|u| u.name.to_lowercase());
                if agents.is_empty() {
                    println!("No agents");
                }
                let tokens = db.agent_tokens()?;
                for a in agents {
                    let mut flags = Vec::new();
                    if a.disabled {
                        flags.push("disabled".to_string());
                    }
                    if agent::check_role(&cfg, &a.role).is_err() {
                        flags.push("role missing from config".into());
                    }
                    if cfg.mcp.health_roles.contains(&a.role) {
                        flags.push("health tools".into());
                    }
                    println!("{:<20} {:<12} {}", a.name, a.role, flags.join(", "));
                    let key = User::key(&a.name);
                    for (_, t) in tokens.iter().filter(|(_, t)| t.agent == key) {
                        let used = match (t.last_used, &t.last_ip) {
                            (Some(u), Some(ip)) => format!("last used {} from {ip}", date(u)),
                            (Some(u), None) => format!("last used {}", date(u)),
                            _ => "never used".to_string(),
                        };
                        let expiry = match t.expires {
                            Some(e) if e <= now => format!(", expired {}", date(e)),
                            Some(e) => format!(", expires {}", date(e)),
                            None => String::new(),
                        };
                        println!(
                            "    token {:<12} {:<30} created {}{expiry}, {used}",
                            t.id,
                            t.label,
                            date(t.created)
                        );
                    }
                }
                ExitCode::SUCCESS
            }
            AgentCmd::Role { name, role } => {
                let a = agent::get(&db, &name)?;
                agent::check_role(&cfg, &role)?;
                db.update_user(&a.name, |u| u.role = role.clone())?;
                audit("agent_role", &User::key(&a.name), &role)?;
                println!("{} now has role {role}", a.name);
                ExitCode::SUCCESS
            }
            AgentCmd::Disable { name } => {
                let a = agent::get(&db, &name)?;
                db.update_user(&a.name, |u| u.disabled = true)?;
                audit("agent_disabled", &User::key(&a.name), "")?;
                println!(
                    "{} is disabled: its tokens stop working and its shares pause",
                    a.name
                );
                ExitCode::SUCCESS
            }
            AgentCmd::Enable { name } => {
                let a = agent::get(&db, &name)?;
                db.update_user(&a.name, |u| u.disabled = false)?;
                audit("agent_enabled", &User::key(&a.name), "")?;
                println!("{} is enabled", a.name);
                ExitCode::SUCCESS
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
                        return Ok(fail("--expires takes an age like 90d or 12h".to_string()));
                    }
                };
                let (token, rec) = agent::create_token(&db, &name, &label, expires, "cli", now)?;
                audit(
                    "agent_token_created",
                    &rec.agent,
                    &format!("{} {}", rec.id, rec.label),
                )?;
                println!(
                    "MCP token \"{}\" (id {}) for agent {}:",
                    rec.label, rec.id, rec.agent
                );
                println!("{token}");
                println!("It is shown only now. Send it as \"Authorization: Bearer <token>\".");
                if let Some(e) = rec.expires {
                    println!("It stops working {}.", date(e));
                }
                ExitCode::SUCCESS
            }
            AgentCmd::Revoke { name, which } => {
                let a = agent::get(&db, &name)?;
                match agent::revoke(&db, &User::key(&a.name), &which)? {
                    Some(r) => {
                        audit(
                            "agent_token_revoked",
                            &r.agent,
                            &format!("{} {}", r.id, r.label),
                        )?;
                        println!("Revoked \"{}\" (id {}) of {}", r.label, r.id, a.name);
                        ExitCode::SUCCESS
                    }
                    None => fail(format!("{} has no token \"{which}\"", a.name)),
                }
            }
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
