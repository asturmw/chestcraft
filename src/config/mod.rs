//! The configuration file: parsing and validation.
//!
//! Parsing is strict (unknown keys are errors) and validation collects every problem,
//! so `chestcraft check` can report them all at once. Checks that look at the machine
//! (do the volume directories exist?) live in [`check_environment`].

pub mod secrets;

use crate::auth::perm::Perms;
use crate::fs::path::RelPath;
use crate::tools;
use ipnet::IpNet;
use jiff::civil::Weekday;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, SocketAddr};
use std::path::{Component, Path, PathBuf};

pub const DEFAULT_CONFIG_PATH: &str = "/etc/chestcraft/config.toml";

/// URL names chestcraft routes itself, so no volume may use them (compared case-insensitively).
pub const RESERVED_VOLUME_NAMES: &[&str] = &["s", "api", "healthz", "robots.txt", "favicon.ico"];

/// System directories a volume may never be, or contain.
const FORBIDDEN_VOLUME_ROOTS: &[&str] = &[
    "/", "/bin", "/boot", "/dev", "/etc", "/lib", "/lib64", "/proc", "/root", "/run", "/sbin",
    "/sys", "/usr", "/var",
];

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}")]
    Syntax(String),
    #[error("{}", .0.join("\n"))]
    Invalid(Vec<String>),
}

// ---------------------------------------------------------------------------------------
// Validated configuration

#[derive(Debug, Clone)]
pub struct Config {
    pub server: ServerConfig,
    pub paths: PathsConfig,
    pub trash: TrashConfig,
    pub uploads: UploadsConfig,
    pub thumbnails: ThumbnailsConfig,
    pub sessions: SessionsConfig,
    pub limits: LimitsConfig,
    pub volumes: BTreeMap<String, VolumeConfig>,
    pub roles: BTreeMap<String, RoleConfig>,
    /// Disk serial number → bay label.
    pub disks: BTreeMap<String, String>,
    pub alerts: AlertsConfig,
    pub shares: SharesConfig,
}

#[derive(Debug, Clone, Default)]
pub struct SharesConfig {
    /// Extra passphrase word lists: language code → file.
    pub wordlists: BTreeMap<String, PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub listen: SocketAddr,
    /// Scheme + host (+ port), no trailing slash.
    pub public_url: String,
    pub trusted_proxies: Vec<IpAddr>,
    pub remote_proxies: Vec<IpAddr>,
    pub local_networks: Vec<IpNet>,
    pub timezone_name: String,
    pub timezone: jiff::tz::TimeZone,
}

#[derive(Debug, Clone)]
pub struct PathsConfig {
    pub state_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub backup_dir: PathBuf,
    /// SMART data written by `chestcraft collect` (root) and read by the server.
    pub smart_file: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeOfDay {
    pub hour: u8,
    pub minute: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WeeklyTime {
    pub weekday: Weekday,
    pub time: TimeOfDay,
}

#[derive(Debug, Clone, Copy)]
pub struct TrashConfig {
    /// 0 = never purge.
    pub days: u32,
    pub purge_at: TimeOfDay,
}

#[derive(Debug, Clone, Copy)]
pub struct UploadsConfig {
    pub chunk_mib: u32,
    pub parallel: u32,
    pub abandoned_days: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct ThumbnailsConfig {
    pub cache_gib: u32,
    pub max_ffmpeg: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct SessionsConfig {
    pub user_days: u32,
    pub remote_admin_hours: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct LimitsConfig {
    pub anon_downloads_per_ip: u32,
}

#[derive(Debug, Clone)]
pub struct VolumeConfig {
    pub name: String,
    pub path: PathBuf,
    pub public: bool,
    pub remote: bool,
    pub read_only: bool,
    pub read_only_paths: Vec<RelPath>,
    pub trash_days: Option<u32>,
    pub stage_folder_uploads: bool,
    pub note: Option<Note>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub de: Option<String>,
    pub en: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RoleConfig {
    pub name: String,
    pub volumes: BTreeMap<String, Perms>,
}

#[derive(Debug, Clone)]
pub struct AlertsConfig {
    /// Language code for alert messages ("de", "en", or an extra language file).
    pub language: String,
    pub digest: WeeklyTime,
    /// Set when `alerts.telegram.chat_id` is configured; the bot token is a secret.
    pub telegram: Option<TelegramConfig>,
    /// Set when `alerts.ntfy.topic` is configured; an access token, if any, is a secret.
    pub ntfy: Option<NtfyConfig>,
    pub thresholds: Thresholds,
    /// Dataset → longest allowed age of its newest snapshot, in seconds. A rule covers the
    /// dataset and everything below it; the longest matching name wins; 0 turns it off.
    pub snapshots: BTreeMap<String, i64>,
}

/// Alert limits. Percentages and temperatures alert when the value is above the limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Thresholds {
    pub pool_warn: u8,
    pub pool_crit: u8,
    pub quota: u8,
    pub root_fs: u8,
    pub scrub_days: u32,
    pub hdd_temp: i32,
    pub ssd_temp: i32,
    pub ssd_wear: u8,
    pub stale_minutes: u32,
}

impl Default for Thresholds {
    fn default() -> Self {
        Thresholds {
            pool_warn: 80,
            pool_crit: 90,
            quota: 90,
            root_fs: 85,
            scrub_days: 40,
            hdd_temp: 50,
            ssd_temp: 70,
            ssd_wear: 80,
            stale_minutes: 120,
        }
    }
}

impl AlertsConfig {
    /// Snapshot age limit for `dataset` (seconds), if a rule covers it.
    pub fn snapshot_limit(&self, dataset: &str) -> Option<i64> {
        self.snapshots
            .iter()
            .filter(|(name, _)| {
                dataset == name.as_str()
                    || dataset
                        .strip_prefix(name.as_str())
                        .is_some_and(|rest| rest.starts_with('/'))
            })
            .max_by_key(|(name, _)| name.len())
            .map(|(_, secs)| *secs)
            .filter(|secs| *secs > 0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelegramConfig {
    pub chat_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NtfyConfig {
    /// Server base URL, e.g. `https://ntfy.sh`.
    pub server: String,
    pub topic: String,
}

impl VolumeConfig {
    /// Trash retention for this volume in days (0 = never purge).
    pub fn trash_days(&self, cfg: &Config) -> u32 {
        self.trash_days.unwrap_or(cfg.trash.days)
    }
}

// ---------------------------------------------------------------------------------------
// Raw file format

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    server: RawServer,
    #[serde(default)]
    paths: RawPaths,
    #[serde(default)]
    trash: RawTrash,
    #[serde(default)]
    uploads: RawUploads,
    #[serde(default)]
    thumbnails: RawThumbnails,
    #[serde(default)]
    sessions: RawSessions,
    #[serde(default)]
    limits: RawLimits,
    #[serde(default)]
    volumes: BTreeMap<String, RawVolume>,
    #[serde(default)]
    roles: BTreeMap<String, BTreeMap<String, String>>,
    #[serde(default)]
    disks: BTreeMap<String, String>,
    #[serde(default)]
    alerts: RawAlerts,
    #[serde(default)]
    shares: RawShares,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
struct RawShares {
    wordlists: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawServer {
    #[serde(default = "default_listen")]
    listen: String,
    public_url: String,
    #[serde(default)]
    trusted_proxies: Vec<String>,
    #[serde(default)]
    remote_proxies: Vec<String>,
    #[serde(default = "default_local_networks")]
    local_networks: Vec<String>,
    #[serde(default = "default_timezone")]
    timezone: String,
}

fn default_listen() -> String {
    "0.0.0.0:3923".into()
}
fn default_local_networks() -> Vec<String> {
    [
        "192.168.0.0/16",
        "10.0.0.0/8",
        "172.16.0.0/12",
        "100.64.0.0/10",
        "127.0.0.0/8",
        "::1/128",
        "fc00::/7",
    ]
    .map(String::from)
    .to_vec()
}
fn default_timezone() -> String {
    "Europe/Berlin".into()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, default)]
struct RawPaths {
    state_dir: String,
    cache_dir: String,
    backup_dir: String,
    smart_file: String,
}

impl Default for RawPaths {
    fn default() -> Self {
        RawPaths {
            state_dir: "/var/lib/chestcraft".into(),
            cache_dir: "/var/cache/chestcraft".into(),
            backup_dir: "/srv/chestcraft".into(),
            smart_file: "/run/chestcraft-smart/smart.json".into(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, default)]
struct RawTrash {
    days: u32,
    purge_at: String,
}

impl Default for RawTrash {
    fn default() -> Self {
        RawTrash {
            days: 30,
            purge_at: "04:00".into(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, default)]
struct RawUploads {
    chunk_mib: u32,
    parallel: u32,
    abandoned_days: u32,
}

impl Default for RawUploads {
    fn default() -> Self {
        RawUploads {
            chunk_mib: 32,
            parallel: 3,
            abandoned_days: 7,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, default)]
struct RawThumbnails {
    cache_gib: u32,
    max_ffmpeg: u32,
}

impl Default for RawThumbnails {
    fn default() -> Self {
        RawThumbnails {
            cache_gib: 5,
            max_ffmpeg: 2,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, default)]
struct RawSessions {
    user_days: u32,
    remote_admin_hours: u32,
}

impl Default for RawSessions {
    fn default() -> Self {
        RawSessions {
            user_days: 30,
            remote_admin_hours: 12,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, default)]
struct RawLimits {
    anon_downloads_per_ip: u32,
}

impl Default for RawLimits {
    fn default() -> Self {
        RawLimits {
            anon_downloads_per_ip: 4,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawVolume {
    path: String,
    #[serde(default)]
    public: bool,
    #[serde(default = "yes")]
    remote: bool,
    #[serde(default)]
    read_only: bool,
    #[serde(default)]
    read_only_paths: Vec<String>,
    trash_days: Option<u32>,
    #[serde(default)]
    stage_folder_uploads: bool,
    note: Option<RawNote>,
}

fn yes() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawNote {
    de: Option<String>,
    en: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, default)]
struct RawAlerts {
    language: String,
    digest: String,
    telegram: RawTelegram,
    ntfy: RawNtfy,
    thresholds: RawThresholds,
    snapshots: BTreeMap<String, String>,
}

impl Default for RawAlerts {
    fn default() -> Self {
        RawAlerts {
            language: "en".into(),
            digest: "Sun 10:00".into(),
            telegram: RawTelegram::default(),
            ntfy: RawNtfy::default(),
            thresholds: RawThresholds::default(),
            snapshots: BTreeMap::new(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, default)]
struct RawThresholds {
    pool_full_warn: u32,
    pool_full_crit: u32,
    quota_full: u32,
    root_fs_full: u32,
    scrub_days: u32,
    hdd_temp: u32,
    ssd_temp: u32,
    ssd_wear: u32,
    stale_minutes: u32,
}

impl Default for RawThresholds {
    fn default() -> Self {
        let d = Thresholds::default();
        RawThresholds {
            pool_full_warn: d.pool_warn.into(),
            pool_full_crit: d.pool_crit.into(),
            quota_full: d.quota.into(),
            root_fs_full: d.root_fs.into(),
            scrub_days: d.scrub_days,
            hdd_temp: d.hdd_temp as u32,
            ssd_temp: d.ssd_temp as u32,
            ssd_wear: d.ssd_wear.into(),
            stale_minutes: d.stale_minutes,
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
struct RawTelegram {
    chat_id: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, default)]
struct RawNtfy {
    server: String,
    topic: String,
}

impl Default for RawNtfy {
    fn default() -> Self {
        RawNtfy {
            server: "https://ntfy.sh".into(),
            topic: String::new(),
        }
    }
}

/// "de", "en", "pt-BR" style codes.
pub fn is_language_code(s: &str) -> bool {
    let (lang, region) = match s.split_once('-') {
        Some((l, r)) => (l, Some(r)),
        None => (s, None),
    };
    (2..=3).contains(&lang.len())
        && lang.chars().all(|c| c.is_ascii_lowercase())
        && region.is_none_or(|r| r.len() == 2 && r.chars().all(|c| c.is_ascii_uppercase()))
}

// ---------------------------------------------------------------------------------------
// Loading

impl Config {
    /// Read and validate a config file.
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&text, path.parent())
    }

    /// Validate config text (tests, `chestcraft check` on stdin).
    pub fn from_toml_str(text: &str) -> Result<Config, ConfigError> {
        Self::parse(text, None)
    }

    fn parse(text: &str, config_dir: Option<&Path>) -> Result<Config, ConfigError> {
        let raw: RawConfig =
            toml::from_str(text).map_err(|e| ConfigError::Syntax(e.to_string()))?;
        let mut errs = Vec::new();
        let cfg = validate(raw, config_dir, &mut errs);
        match cfg {
            Some(cfg) if errs.is_empty() => Ok(cfg),
            _ => Err(ConfigError::Invalid(errs)),
        }
    }
}

fn validate(raw: RawConfig, config_dir: Option<&Path>, errs: &mut Vec<String>) -> Option<Config> {
    let server = validate_server(raw.server, errs);
    let paths = validate_paths(&raw.paths, errs);
    let trash = TrashConfig {
        days: in_range(raw.trash.days, 0, 3650, "trash.days", errs),
        purge_at: TimeOfDay::parse(&raw.trash.purge_at)
            .map_err(|e| errs.push(format!("trash.purge_at: {e}")))
            .unwrap_or(TimeOfDay { hour: 4, minute: 0 }),
    };
    let uploads = UploadsConfig {
        // Cloudflare caps request bodies at 100 MB, so chunks stay well below that.
        chunk_mib: in_range(raw.uploads.chunk_mib, 1, 64, "uploads.chunk_mib", errs),
        parallel: in_range(raw.uploads.parallel, 1, 8, "uploads.parallel", errs),
        abandoned_days: in_range(
            raw.uploads.abandoned_days,
            1,
            365,
            "uploads.abandoned_days",
            errs,
        ),
    };
    let thumbnails = ThumbnailsConfig {
        cache_gib: in_range(
            raw.thumbnails.cache_gib,
            0,
            1024,
            "thumbnails.cache_gib",
            errs,
        ),
        max_ffmpeg: in_range(
            raw.thumbnails.max_ffmpeg,
            1,
            16,
            "thumbnails.max_ffmpeg",
            errs,
        ),
    };
    let sessions = SessionsConfig {
        user_days: in_range(raw.sessions.user_days, 1, 365, "sessions.user_days", errs),
        remote_admin_hours: in_range(
            raw.sessions.remote_admin_hours,
            1,
            720,
            "sessions.remote_admin_hours",
            errs,
        ),
    };
    let limits = LimitsConfig {
        anon_downloads_per_ip: in_range(
            raw.limits.anon_downloads_per_ip,
            1,
            64,
            "limits.anon_downloads_per_ip",
            errs,
        ),
    };

    let volumes = validate_volumes(raw.volumes, paths.as_ref(), config_dir, errs);
    let roles = validate_roles(raw.roles, &volumes, errs);

    let mut disks = BTreeMap::new();
    for (serial, label) in raw.disks {
        let (serial, label) = (serial.trim().to_string(), label.trim().to_string());
        if serial.is_empty() || label.is_empty() || label.chars().count() > 64 {
            errs.push(format!(
                "disks: \"{serial}\" needs a label of 1–64 characters"
            ));
        } else {
            disks.insert(serial, label);
        }
    }

    let language = raw.alerts.language.trim().to_string();
    if !is_language_code(&language) {
        errs.push(format!(
            "alerts.language: \"{language}\" is not a language code like \"de\" or \"en\""
        ));
    }
    let ntfy_topic = raw.alerts.ntfy.topic.trim().to_string();
    let ntfy = if ntfy_topic.is_empty() {
        None
    } else {
        if ntfy_topic.len() > 64
            || !ntfy_topic
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            errs.push(format!(
                "alerts.ntfy.topic: \"{ntfy_topic}\" may only use letters, digits, - and _ (max 64)"
            ));
        }
        let server = raw
            .alerts
            .ntfy
            .server
            .trim()
            .trim_end_matches('/')
            .to_string();
        match server.parse::<http::Uri>() {
            Ok(u)
                if matches!(u.scheme_str(), Some("http" | "https")) && u.authority().is_some() => {}
            _ => errs.push(format!(
                "alerts.ntfy.server: \"{server}\" is not an http(s) URL"
            )),
        }
        Some(NtfyConfig {
            server,
            topic: ntfy_topic,
        })
    };
    let alerts = AlertsConfig {
        language,
        digest: WeeklyTime::parse(&raw.alerts.digest)
            .map_err(|e| errs.push(format!("alerts.digest: {e}")))
            .unwrap_or(WeeklyTime {
                weekday: Weekday::Sunday,
                time: TimeOfDay {
                    hour: 10,
                    minute: 0,
                },
            }),
        telegram: (raw.alerts.telegram.chat_id != 0).then_some(TelegramConfig {
            chat_id: raw.alerts.telegram.chat_id,
        }),
        ntfy,
        thresholds: validate_thresholds(&raw.alerts.thresholds, errs),
        snapshots: raw
            .alerts
            .snapshots
            .iter()
            .filter_map(|(dataset, age)| {
                let name = dataset.trim().trim_end_matches('/');
                if name.is_empty() || name.contains('@') || name.starts_with('/') {
                    errs.push(format!(
                        "alerts.snapshots: \"{dataset}\" is not a dataset name like \"tank/data\""
                    ));
                    return None;
                }
                match parse_age(age) {
                    Ok(secs) => Some((name.to_string(), secs)),
                    Err(e) => {
                        errs.push(format!("alerts.snapshots.\"{dataset}\": {e}"));
                        None
                    }
                }
            })
            .collect(),
    };

    let mut wordlists = BTreeMap::new();
    for (lang, path) in raw.shares.wordlists {
        if !is_language_code(&lang) {
            errs.push(format!(
                "shares.wordlists: \"{lang}\" is not a language code like \"de\" or \"en\""
            ));
            continue;
        }
        match abs_path(&path) {
            Ok(p) => {
                wordlists.insert(lang, p);
            }
            Err(e) => errs.push(format!("shares.wordlists.{lang}: {e}")),
        }
    }

    Some(Config {
        server: server?,
        paths: paths?,
        trash,
        uploads,
        thumbnails,
        sessions,
        limits,
        volumes,
        roles,
        disks,
        alerts,
        shares: SharesConfig { wordlists },
    })
}

fn validate_thresholds(raw: &RawThresholds, errs: &mut Vec<String>) -> Thresholds {
    let pct = |v: u32, key: &str, errs: &mut Vec<String>| {
        in_range(v, 1, 100, &format!("alerts.thresholds.{key}"), errs) as u8
    };
    let pool_warn = pct(raw.pool_full_warn, "pool_full_warn", errs);
    let pool_crit = pct(raw.pool_full_crit, "pool_full_crit", errs);
    if pool_crit < pool_warn {
        errs.push("alerts.thresholds: pool_full_crit is below pool_full_warn".into());
    }
    Thresholds {
        pool_warn,
        pool_crit,
        quota: pct(raw.quota_full, "quota_full", errs),
        root_fs: pct(raw.root_fs_full, "root_fs_full", errs),
        scrub_days: in_range(
            raw.scrub_days,
            1,
            3650,
            "alerts.thresholds.scrub_days",
            errs,
        ),
        hdd_temp: in_range(raw.hdd_temp, 20, 120, "alerts.thresholds.hdd_temp", errs) as i32,
        ssd_temp: in_range(raw.ssd_temp, 20, 120, "alerts.thresholds.ssd_temp", errs) as i32,
        ssd_wear: pct(raw.ssd_wear, "ssd_wear", errs),
        stale_minutes: in_range(
            raw.stale_minutes,
            10,
            7 * 24 * 60,
            "alerts.thresholds.stale_minutes",
            errs,
        ),
    }
}

/// "90m", "2h", "2d", or "0" (off) → seconds.
pub fn parse_age(s: &str) -> Result<i64, String> {
    let s = s.trim();
    if s == "0" {
        return Ok(0);
    }
    let cut = s.char_indices().last().map_or(0, |(i, _)| i);
    let (num, unit) = s.split_at(cut);
    let n: i64 = num
        .trim()
        .parse()
        .ok()
        .filter(|n| (1..=100_000).contains(n))
        .ok_or_else(|| format!("\"{s}\" is not an age like \"90m\", \"2h\" or \"2d\""))?;
    match unit {
        "m" => Ok(n * 60),
        "h" => Ok(n * 3600),
        "d" => Ok(n * 86_400),
        _ => Err(format!(
            "\"{s}\" is not an age like \"90m\", \"2h\" or \"2d\""
        )),
    }
}

fn in_range(v: u32, min: u32, max: u32, key: &str, errs: &mut Vec<String>) -> u32 {
    if v < min || v > max {
        errs.push(format!("{key}: {v} is outside {min}–{max}"));
    }
    v.clamp(min, max)
}

fn validate_server(raw: RawServer, errs: &mut Vec<String>) -> Option<ServerConfig> {
    let listen = raw
        .listen
        .parse::<SocketAddr>()
        .map_err(|_| {
            errs.push(format!(
                "server.listen: \"{}\" is not an address:port",
                raw.listen
            ))
        })
        .ok();

    let public_url = validate_public_url(&raw.public_url)
        .map_err(|e| errs.push(format!("server.public_url: {e}")))
        .ok();

    let parse_ips = |key: &str, list: &[String], errs: &mut Vec<String>| -> Vec<IpAddr> {
        list.iter()
            .filter_map(|s| {
                s.parse::<IpAddr>()
                    .map_err(|_| errs.push(format!("{key}: \"{s}\" is not an IP address")))
                    .ok()
            })
            .collect()
    };
    let trusted_proxies = parse_ips("server.trusted_proxies", &raw.trusted_proxies, errs);
    let remote_proxies = parse_ips("server.remote_proxies", &raw.remote_proxies, errs);
    for ip in &remote_proxies {
        if !trusted_proxies.contains(ip) {
            errs.push(format!(
                "server.remote_proxies: {ip} must also be listed in server.trusted_proxies"
            ));
        }
    }

    let local_networks = raw
        .local_networks
        .iter()
        .filter_map(|s| {
            s.parse::<IpNet>()
                .or_else(|_| s.parse::<IpAddr>().map(IpNet::from))
                .map_err(|_| {
                    errs.push(format!(
                        "server.local_networks: \"{s}\" is not a network or address"
                    ))
                })
                .ok()
        })
        .collect();

    let timezone = jiff::tz::TimeZone::get(&raw.timezone)
        .map_err(|_| {
            errs.push(format!(
                "server.timezone: unknown time zone \"{}\"",
                raw.timezone
            ))
        })
        .ok();

    Some(ServerConfig {
        listen: listen?,
        public_url: public_url?,
        trusted_proxies,
        remote_proxies,
        local_networks,
        timezone_name: raw.timezone,
        timezone: timezone?,
    })
}

fn validate_public_url(s: &str) -> Result<String, String> {
    let s = s.trim().trim_end_matches('/');
    let uri: http::Uri = s.parse().map_err(|_| format!("\"{s}\" is not a URL"))?;
    match uri.scheme_str() {
        Some("http" | "https") => {}
        _ => return Err(format!("\"{s}\" must start with http:// or https://")),
    }
    if uri.authority().is_none() {
        return Err(format!("\"{s}\" has no host"));
    }
    if !matches!(uri.path(), "" | "/") || uri.query().is_some() {
        return Err(format!("\"{s}\" must not have a path or query"));
    }
    Ok(s.to_string())
}

/// An absolute path without `.`/`..` components, rebuilt in normal form.
fn abs_path(s: &str) -> Result<PathBuf, String> {
    let p = Path::new(s);
    if !p.is_absolute() {
        return Err(format!("\"{s}\" is not an absolute path"));
    }
    let mut out = PathBuf::from("/");
    for c in p.components() {
        match c {
            Component::RootDir => {}
            Component::Normal(n) => out.push(n),
            _ => return Err(format!("\"{s}\" contains . or .. components")),
        }
    }
    Ok(out)
}

fn validate_paths(raw: &RawPaths, errs: &mut Vec<String>) -> Option<PathsConfig> {
    let mut get = |key: &str, s: &str| {
        abs_path(s)
            .map_err(|e| errs.push(format!("paths.{key}: {e}")))
            .ok()
    };
    let state_dir = get("state_dir", &raw.state_dir);
    let cache_dir = get("cache_dir", &raw.cache_dir);
    let backup_dir = get("backup_dir", &raw.backup_dir);
    let smart_file = get("smart_file", &raw.smart_file);
    Some(PathsConfig {
        state_dir: state_dir?,
        cache_dir: cache_dir?,
        backup_dir: backup_dir?,
        smart_file: smart_file?,
    })
}

fn valid_volume_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 64 {
        return Err("names need 1–64 bytes".into());
    }
    if name.starts_with('.') || name.starts_with(' ') || name.ends_with(' ') {
        return Err("names can't start with a dot or start/end with a space".into());
    }
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || " -_.".contains(c))
    {
        return Err("use letters, digits, space, - _ . only".into());
    }
    if RESERVED_VOLUME_NAMES
        .iter()
        .any(|r| r.eq_ignore_ascii_case(name))
    {
        return Err(format!("\"{name}\" is reserved for chestcraft's own URLs"));
    }
    Ok(())
}

fn validate_volumes(
    raw: BTreeMap<String, RawVolume>,
    paths: Option<&PathsConfig>,
    config_dir: Option<&Path>,
    errs: &mut Vec<String>,
) -> BTreeMap<String, VolumeConfig> {
    let mut out = BTreeMap::new();
    let mut seen_lower = BTreeSet::new();
    for (name, v) in raw {
        let key = format!("volumes.{name}");
        if let Err(e) = valid_volume_name(&name) {
            errs.push(format!("{key}: {e}"));
            continue;
        }
        if !seen_lower.insert(name.to_lowercase()) {
            errs.push(format!(
                "{key}: another volume has the same name in different case"
            ));
            continue;
        }
        let path = match abs_path(&v.path) {
            Ok(p) => p,
            Err(e) => {
                errs.push(format!("{key}.path: {e}"));
                continue;
            }
        };
        if let Some(root) = FORBIDDEN_VOLUME_ROOTS.iter().find(|root| {
            path == Path::new(root) || (**root != "/" && Path::new(root).starts_with(&path))
        }) {
            errs.push(format!(
                "{key}.path: {} is or contains the system directory {root}",
                path.display()
            ));
        }
        if let Some(paths) = paths {
            for (what, dir) in [
                ("state_dir", &paths.state_dir),
                ("cache_dir", &paths.cache_dir),
            ] {
                if dir.starts_with(&path) || path.starts_with(dir) {
                    errs.push(format!(
                        "{key}.path: overlaps paths.{what} ({}); state must never be served",
                        dir.display()
                    ));
                }
            }
            if paths.backup_dir.starts_with(&path) {
                errs.push(format!(
                    "{key}.path: contains paths.backup_dir ({}); database copies must never be served",
                    paths.backup_dir.display()
                ));
            }
        }
        if let Some(cdir) = config_dir
            && cdir.starts_with(&path)
        {
            errs.push(format!(
                "{key}.path: contains the config directory {}",
                cdir.display()
            ));
        }
        let mut read_only_paths = Vec::new();
        for ro in &v.read_only_paths {
            match RelPath::parse(ro) {
                Ok(p) if !p.is_root() => read_only_paths.push(p),
                Ok(_) => errs.push(format!(
                    "{key}.read_only_paths: \"{ro}\" is the whole volume; use read_only = true"
                )),
                Err(e) => errs.push(format!("{key}.read_only_paths: \"{ro}\": {e}")),
            }
        }
        let note = v.note.map(|n| Note {
            de: n.de.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
            en: n.en.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
        });
        if let Some(Note { de: None, en: None }) = note {
            errs.push(format!("{key}.note: set note.de and/or note.en"));
        }
        if let Some(d) = v.trash_days {
            in_range(d, 0, 3650, &format!("{key}.trash_days"), errs);
        }
        out.insert(
            name.clone(),
            VolumeConfig {
                name,
                path,
                public: v.public,
                remote: v.remote,
                read_only: v.read_only,
                read_only_paths,
                trash_days: v.trash_days,
                stage_folder_uploads: v.stage_folder_uploads,
                note,
            },
        );
    }
    out
}

fn validate_roles(
    raw: BTreeMap<String, BTreeMap<String, String>>,
    volumes: &BTreeMap<String, VolumeConfig>,
    errs: &mut Vec<String>,
) -> BTreeMap<String, RoleConfig> {
    let mut out = BTreeMap::new();
    for (name, grants) in raw {
        let key = format!("roles.{name}");
        if name == "admin" {
            errs.push(format!(
                "{key}: \"admin\" is built in and can't be redefined"
            ));
            continue;
        }
        if name.is_empty()
            || name.len() > 32
            || !name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        {
            errs.push(format!(
                "{key}: role names use a–z, 0–9, - and _ (1–32 characters)"
            ));
            continue;
        }
        let mut vols = BTreeMap::new();
        for (vol, letters) in grants {
            if !volumes.contains_key(&vol) {
                errs.push(format!("{key}: unknown volume \"{vol}\""));
                continue;
            }
            match Perms::parse(&letters) {
                Ok(p) => {
                    vols.insert(vol, p);
                }
                Err(e) => errs.push(format!("{key}.{vol}: {e}")),
            }
        }
        out.insert(
            name.clone(),
            RoleConfig {
                name,
                volumes: vols,
            },
        );
    }
    out
}

impl TimeOfDay {
    pub fn parse(s: &str) -> Result<TimeOfDay, String> {
        let bad = || format!("\"{s}\" is not a time like 04:00");
        let (h, m) = s.trim().split_once(':').ok_or_else(bad)?;
        let hour: u8 = h.parse().map_err(|_| bad())?;
        let minute: u8 = m.parse().map_err(|_| bad())?;
        if hour > 23 || minute > 59 || m.len() != 2 {
            return Err(bad());
        }
        Ok(TimeOfDay { hour, minute })
    }
}

impl WeeklyTime {
    pub fn parse(s: &str) -> Result<WeeklyTime, String> {
        let bad = || format!("\"{s}\" is not like \"Sun 10:00\"");
        let (day, time) = s.trim().split_once(' ').ok_or_else(bad)?;
        let weekday = match day.to_ascii_lowercase().as_str() {
            "mon" | "monday" => Weekday::Monday,
            "tue" | "tuesday" => Weekday::Tuesday,
            "wed" | "wednesday" => Weekday::Wednesday,
            "thu" | "thursday" => Weekday::Thursday,
            "fri" | "friday" => Weekday::Friday,
            "sat" | "saturday" => Weekday::Saturday,
            "sun" | "sunday" => Weekday::Sunday,
            _ => return Err(bad()),
        };
        Ok(WeeklyTime {
            weekday,
            time: TimeOfDay::parse(time.trim())?,
        })
    }
}

// ---------------------------------------------------------------------------------------
// Environment checks

#[derive(Debug, Default)]
pub struct EnvReport {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

/// Checks against the machine the config will run on.
pub fn check_environment(cfg: &Config) -> EnvReport {
    let mut r = EnvReport::default();
    for v in cfg.volumes.values() {
        if !v.path.is_dir() {
            r.errors.push(format!(
                "volumes.{}: {} is not a directory",
                v.name,
                v.path.display()
            ));
            continue;
        }
        if !v.read_only {
            for dir in [crate::fs::ops::TRASH_DIR, crate::fs::ops::UPLOADS_DIR] {
                let p = v.path.join(dir);
                let writable = rustix::fs::access(&v.path, rustix::fs::Access::WRITE_OK).is_ok();
                if !p.is_dir() && !writable {
                    r.warnings.push(format!(
                        "volumes.{}: {} doesn't exist and chestcraft can't create it; create it \
                         (owned by the service user) so deleting and uploading work",
                        v.name,
                        p.display()
                    ));
                }
            }
        }
        for ro in &v.read_only_paths {
            if !v.path.join(ro.as_str()).exists() {
                r.warnings.push(format!(
                    "volumes.{}: read-only area {} doesn't exist",
                    v.name, ro
                ));
            }
        }
    }
    for (what, dir) in [
        ("state_dir", &cfg.paths.state_dir),
        ("cache_dir", &cfg.paths.cache_dir),
        ("backup_dir", &cfg.paths.backup_dir),
    ] {
        if !dir.is_dir() {
            r.warnings
                .push(format!("paths.{what}: {} doesn't exist yet", dir.display()));
        }
    }
    for (lang, path) in &cfg.shares.wordlists {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                if let Err(e) = crate::shares::words::check(&text) {
                    r.errors
                        .push(format!("shares.wordlists.{lang}: {}: {e}", path.display()));
                }
            }
            Err(e) => r.errors.push(format!(
                "shares.wordlists.{lang}: can't read {}: {e}",
                path.display()
            )),
        }
    }
    for tool in tools::ALL {
        if !Path::new(tool).exists() {
            r.warnings.push(format!(
                "{tool} is missing; the features that use it stay off"
            ));
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
        [server]
        public_url = "https://files.example.org/"
    "#;

    fn errors_of(text: &str) -> Vec<String> {
        match Config::from_toml_str(text) {
            Err(ConfigError::Invalid(e)) => e,
            Err(other) => vec![other.to_string()],
            Ok(_) => vec![],
        }
    }

    fn with_volumes(extra: &str) -> String {
        format!("{MINIMAL}\n{extra}")
    }

    #[test]
    fn example_config_is_valid() {
        let text = include_str!("../../config.example.toml");
        let cfg =
            Config::from_toml_str(text).unwrap_or_else(|e| panic!("config.example.toml: {e}"));
        assert!(cfg.volumes.contains_key("Eingang"));
        assert!(cfg.volumes["Austausch"].public);
        assert!(!cfg.volumes["Backups"].remote);
        assert_eq!(cfg.server.public_url, "https://files.example.org");
    }

    #[test]
    fn minimal_config_gets_defaults() {
        let cfg = Config::from_toml_str(MINIMAL).unwrap();
        assert_eq!(cfg.server.listen.port(), 3923);
        assert_eq!(cfg.server.public_url, "https://files.example.org");
        assert_eq!(cfg.trash.days, 30);
        assert_eq!(cfg.trash.purge_at, TimeOfDay { hour: 4, minute: 0 });
        assert_eq!(cfg.uploads.chunk_mib, 32);
        assert_eq!(cfg.alerts.digest.weekday, Weekday::Sunday);
        assert!(cfg.alerts.telegram.is_none() && cfg.alerts.ntfy.is_none());
        assert_eq!(cfg.alerts.language, "en");
        assert_eq!(cfg.paths.state_dir, PathBuf::from("/var/lib/chestcraft"));
    }

    #[test]
    fn unknown_keys_are_errors() {
        let e = errors_of(&with_volumes(
            "[volumes.A]\npath = \"/srv/a\"\npublik = true\n",
        ));
        assert!(e[0].contains("publik"), "{e:?}");
    }

    #[test]
    fn reserved_and_bad_volume_names() {
        let e = errors_of(&with_volumes(
            "[volumes.s]\npath = \"/srv/a\"\n[volumes.Api]\npath = \"/srv/b\"\n",
        ));
        assert_eq!(
            e.iter().filter(|m| m.contains("reserved")).count(),
            2,
            "{e:?}"
        );
        let e = errors_of(&with_volumes("[volumes.\".hidden\"]\npath = \"/srv/a\"\n"));
        assert!(e[0].contains("dot"), "{e:?}");
        let e = errors_of(&with_volumes(
            "[volumes.Share]\npath = \"/srv/a\"\n[volumes.share]\npath = \"/srv/b\"\n",
        ));
        assert!(e[0].contains("different case"), "{e:?}");
    }

    #[test]
    fn state_must_never_be_served() {
        let e = errors_of(&with_volumes("[volumes.All]\npath = \"/srv\"\n"));
        assert!(e.iter().any(|m| m.contains("backup_dir")), "{e:?}");
        let e = errors_of(&with_volumes("[volumes.Var]\npath = \"/var/lib\"\n"));
        assert!(e.iter().any(|m| m.contains("state_dir")), "{e:?}");
        let e = errors_of(&with_volumes("[volumes.Root]\npath = \"/\"\n"));
        assert!(e.iter().any(|m| m.contains("system directory")), "{e:?}");
        // A sandbox volume below the backup dir is fine: it doesn't contain the copies.
        assert!(
            errors_of(&with_volumes(
                "[volumes.Sandbox]\npath = \"/srv/chestcraft/sandbox\"\n"
            ))
            .is_empty()
        );
    }

    #[test]
    fn config_dir_is_never_served() {
        let raw: RawConfig =
            toml::from_str(&with_volumes("[volumes.Opt]\npath = \"/opt\"\n")).unwrap();
        let mut errs = Vec::new();
        validate(raw, Some(Path::new("/opt/chestcraft")), &mut errs);
        assert!(
            errs.iter().any(|m| m.contains("config directory")),
            "{errs:?}"
        );
    }

    #[test]
    fn paths_must_be_clean_and_absolute() {
        let e = errors_of(&with_volumes(
            "[volumes.A]\npath = \"srv/a\"\n[volumes.B]\npath = \"/srv/../etc\"\n",
        ));
        assert_eq!(e.len(), 2, "{e:?}");
        let cfg =
            Config::from_toml_str(&with_volumes("[volumes.A]\npath = \"/srv//a/\"\n")).unwrap();
        assert_eq!(cfg.volumes["A"].path, PathBuf::from("/srv/a"));
    }

    #[test]
    fn roles_reference_existing_volumes_and_valid_letters() {
        let text = with_volumes(
            "[volumes.A]\npath = \"/srv/a\"\n\
             [roles.ok]\nA = \"rwmds\"\n\
             [roles.typo]\nB = \"r\"\n\
             [roles.badletters]\nA = \"ws\"\n\
             [roles.admin]\nA = \"r\"\n\
             [roles.Upper]\nA = \"r\"\n",
        );
        let e = errors_of(&text);
        assert!(
            e.iter().any(|m| m.contains("unknown volume \"B\"")),
            "{e:?}"
        );
        assert!(e.iter().any(|m| m.contains("needs r")), "{e:?}");
        assert!(e.iter().any(|m| m.contains("built in")), "{e:?}");
        assert!(e.iter().any(|m| m.contains("role names")), "{e:?}");
        assert_eq!(e.len(), 4, "{e:?}");
    }

    #[test]
    fn read_only_paths_and_notes() {
        let e = errors_of(&with_volumes(
            "[volumes.A]\npath = \"/srv/a\"\nread_only_paths = [\"\", \"../x\", \"ok/sub\"]\nnote = {}\n",
        ));
        assert_eq!(e.len(), 3, "{e:?}");
        let cfg = Config::from_toml_str(&with_volumes(
            "[volumes.A]\npath = \"/srv/a\"\nread_only_paths = [\"/immich/\"]\nnote.en = \" Hi \"\n",
        ))
        .unwrap();
        assert_eq!(cfg.volumes["A"].read_only_paths[0].as_str(), "immich");
        assert_eq!(
            cfg.volumes["A"].note.as_ref().unwrap().en.as_deref(),
            Some("Hi")
        );
    }

    #[test]
    fn proxies_networks_and_urls() {
        let e = errors_of(
            "[server]\npublic_url = \"ftp://x\"\nremote_proxies = [\"192.0.2.1\"]\nlocal_networks = [\"nope\"]\nlisten = \"3923\"\n",
        );
        assert!(e.iter().any(|m| m.contains("public_url")), "{e:?}");
        assert!(e.iter().any(|m| m.contains("trusted_proxies")), "{e:?}");
        assert!(e.iter().any(|m| m.contains("local_networks")), "{e:?}");
        assert!(e.iter().any(|m| m.contains("listen")), "{e:?}");
        let e = errors_of("[server]\npublic_url = \"https://x.org/sub\"\n");
        assert!(e[0].contains("path"), "{e:?}");
        let cfg = Config::from_toml_str(
            "[server]\npublic_url = \"http://x.org\"\nlocal_networks = [\"10.1.2.3\"]\n",
        )
        .unwrap();
        assert_eq!(cfg.server.local_networks[0].prefix_len(), 32);
    }

    #[test]
    fn share_word_lists() {
        let e = errors_of(&with_volumes(
            "[shares.wordlists]\nFR = \"/x\"\nit = \"rel/words.txt\"\n",
        ));
        assert_eq!(e.len(), 2, "{e:?}");
        let cfg = Config::from_toml_str(&with_volumes(
            "[shares.wordlists]\nfr = \"/etc/chestcraft/words-fr.txt\"\n",
        ))
        .unwrap();
        assert_eq!(
            cfg.shares.wordlists["fr"],
            PathBuf::from("/etc/chestcraft/words-fr.txt")
        );
    }

    #[test]
    fn times_and_ranges() {
        assert_eq!(
            TimeOfDay::parse("23:59").unwrap(),
            TimeOfDay {
                hour: 23,
                minute: 59
            }
        );
        for bad in ["24:00", "4", "04:0", "aa:bb"] {
            assert!(TimeOfDay::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(
            WeeklyTime::parse("sat 01:30").unwrap().weekday,
            Weekday::Saturday
        );
        assert!(WeeklyTime::parse("Someday 10:00").is_err());
        let e = errors_of(
            "[server]\npublic_url = \"http://x.org\"\n[uploads]\nchunk_mib = 100\n[alerts]\nlanguage = \"French\"\n[alerts.ntfy]\ntopic = \"bad topic!\"\n",
        );
        assert!(e.iter().any(|m| m.contains("chunk_mib")), "{e:?}");
        assert!(e.iter().any(|m| m.contains("language")), "{e:?}");
        assert!(e.iter().any(|m| m.contains("ntfy.topic")), "{e:?}");
        let cfg = Config::from_toml_str(
            "[server]\npublic_url = \"http://x.org\"\n[alerts]\nlanguage = \"pt-BR\"\n[alerts.ntfy]\ntopic = \"nas-alerts\"\n[alerts.telegram]\nchat_id = -100123\n",
        )
        .unwrap();
        assert_eq!(cfg.alerts.ntfy.as_ref().unwrap().server, "https://ntfy.sh");
        assert_eq!(cfg.alerts.telegram.as_ref().unwrap().chat_id, -100123);
        assert!(is_language_code("de") && !is_language_code("DE") && !is_language_code("pt-br"));
    }

    #[test]
    fn alert_thresholds_and_snapshot_rules() {
        let cfg = Config::from_toml_str(
            "[server]\npublic_url = \"http://x.org\"\n[alerts.thresholds]\nhdd_temp = 45\n\
             [alerts.snapshots]\n\"tank\" = \"2d\"\n\"tank/data/\" = \"90m\"\n\"tank/scratch\" = \"0\"\n",
        )
        .unwrap();
        let t = cfg.alerts.thresholds;
        assert_eq!((t.hdd_temp, t.pool_warn, t.stale_minutes), (45, 80, 120));
        let a = &cfg.alerts;
        assert_eq!(a.snapshot_limit("tank"), Some(2 * 86_400));
        assert_eq!(a.snapshot_limit("tank/data"), Some(90 * 60));
        assert_eq!(a.snapshot_limit("tank/data/sub"), Some(90 * 60));
        assert_eq!(a.snapshot_limit("tank/database"), Some(2 * 86_400));
        assert_eq!(a.snapshot_limit("tank/scratch/x"), None);
        assert_eq!(a.snapshot_limit("other"), None);
        assert_eq!(
            cfg.paths.smart_file,
            Path::new("/run/chestcraft-smart/smart.json")
        );

        let Err(ConfigError::Invalid(e)) = Config::from_toml_str(
            "[server]\npublic_url = \"http://x.org\"\n[alerts.thresholds]\npool_full_warn = 95\n\
             pool_full_crit = 90\nssd_wear = 0\n[alerts.snapshots]\n\"tank@snap\" = \"1h\"\n\"tank\" = \"soon\"\n",
        ) else {
            panic!("accepted")
        };
        for want in [
            "pool_full_crit is below",
            "ssd_wear",
            "tank@snap",
            "\"soon\"",
        ] {
            assert!(e.iter().any(|m| m.contains(want)), "{want}: {e:?}");
        }
        assert_eq!(parse_age("0"), Ok(0));
        assert_eq!(parse_age(" 3d "), Ok(3 * 86_400));
        assert!(parse_age("3w").is_err() && parse_age("h").is_err() && parse_age("").is_err());
        assert!(parse_age("3ä").is_err());
    }
}
