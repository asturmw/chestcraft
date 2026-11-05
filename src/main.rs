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
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Cmd::Check { offline } => check(&cli.config, offline),
        Cmd::Serve | Cmd::Collect => {
            eprintln!("not implemented yet");
            ExitCode::from(2)
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
