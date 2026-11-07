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
        /// Print a systemd drop-in with the folders the service may write to (for
        /// chestcraft.service.d/; deploy/install.sh writes it)
        #[arg(long, conflicts_with = "healthz_url")]
        systemd: bool,
        /// Print the local /healthz URL (for deploy scripts)
        #[arg(long)]
        healthz_url: bool,
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
    #[command(flatten)]
    Admin(chestcraft::cli::AdminCmd),
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

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Cmd::Check {
            offline,
            systemd,
            healthz_url,
        } => check(&cli.config, offline, systemd, healthz_url),
        Cmd::Serve => serve(&cli.config),
        Cmd::Admin(cmd) => admin(&cli.config, cmd),
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
    let exit = runtime.block_on(chestcraft::web::serve(cfg, path));
    // Tasks still running (a long download, a thumbnail) end with the runtime.
    runtime.shutdown_timeout(std::time::Duration::from_secs(2));
    match exit {
        Ok(chestcraft::web::Exit::Stop) => ExitCode::SUCCESS,
        Ok(chestcraft::web::Exit::Restart) => restart(),
        Err(e) => {
            tracing::error!("{e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Start this same binary again in this process (same pid for systemd, same arguments and
/// environment): `systemctl reload` lands here once the new configuration passed its
/// checks. The database and the listeners close with the old image (close-on-exec).
fn restart() -> ExitCode {
    use std::os::unix::process::CommandExt;
    let mut args = std::env::args_os();
    let argv0 = args.next().unwrap_or_else(|| "chestcraft".into());
    let err = std::process::Command::new("/proc/self/exe")
        .arg0(argv0)
        .args(args)
        .exec();
    tracing::error!("can't restart: {err}");
    ExitCode::FAILURE
}

fn check(path: &Path, offline: bool, systemd: bool, healthz_url: bool) -> ExitCode {
    let cfg = match Config::load(path) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("{}:\n{e}", path.display());
            return ExitCode::FAILURE;
        }
    };
    if systemd {
        print!("{}", config::systemd_dropin(&cfg, path));
        return ExitCode::SUCCESS;
    }
    if healthz_url {
        println!("{}", config::healthz_url(&cfg));
        return ExitCode::SUCCESS;
    }
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

/// Account, agent, token and backup commands: through the running server's admin socket,
/// or on the database directly when no server runs.
fn admin(path: &Path, cmd: chestcraft::cli::AdminCmd) -> ExitCode {
    use chestcraft::cli::{self, Remote};
    let cfg = match Config::load(path) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("{}:\n{e}", path.display());
            return ExitCode::FAILURE;
        }
    };
    let out = match cli::send(&cfg.paths.admin_socket, &cmd) {
        Ok(Remote::Done(out)) => out,
        Ok(Remote::NoServer) => {
            if let Some(code) = refuse_root(cmd.label()) {
                return code;
            }
            let db = match chestcraft::web::open_db(&cfg) {
                Ok(db) => db,
                Err(e) => {
                    eprintln!("{e}");
                    return ExitCode::FAILURE;
                }
            };
            let env = cli::Env {
                cfg: &cfg,
                db: &db,
                config_file: Some(path),
            };
            cli::run(cmd, &env, jiff::Timestamp::now().as_second())
        }
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    print!("{}", out.stdout);
    eprint!("{}", out.stderr);
    ExitCode::from(out.code)
}
