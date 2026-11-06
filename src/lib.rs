//! chestcraft: file sharing for a home NAS.

/// Set by deploy/release.sh from `git describe`; falls back to the crate version.
pub const VERSION: &str = match option_env!("CHESTCRAFT_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

pub mod appearance;
pub mod auth;
pub mod config;
pub mod dav;
pub mod db;
pub mod fs;
pub mod health;
pub mod i18n;
pub mod import;
pub mod mcp;
pub mod notify;
pub mod shares;
pub mod tools;
pub mod web;
