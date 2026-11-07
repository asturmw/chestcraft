//! Alert rules and their state machine.
//!
//! Every check turns a [`Snapshot`] into *conditions* (what is wrong right now, each with a
//! stable key) and *events* (things that happened once: a counter rose). [`step`] compares
//! the conditions with the active alerts: a new key fires (one message), a key that went
//! away resolves (one message), a critical alert repeats every 24 hours, and a rise in
//! severity escalates. Keys of a source whose data is missing this time (pool status timed
//! out, SMART data stale) stay as they are instead of resolving.
//!
//! Messages are stored as an i18n key plus typed arguments and rendered in the reader's
//! language: the alert language for Telegram/ntfy, the admin's for the dashboard.

use super::disks::{Disk, Kind};
use super::{Probe, SmartFile, Snapshot};
use crate::config::{AlertsConfig, Thresholds};
use crate::i18n::{I18n, Lang};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// A critical alert is sent again after this long.
pub const REPEAT_SECS: i64 = 24 * 3600;
/// Baselines of counters not seen for this long are forgotten.
const BASELINE_TTL: i64 = 30 * 86_400;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warning,
    Critical,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Critical => "critical",
        }
    }
}

/// A message argument, formatted for the reader's language.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "t", content = "v", rename_all = "lowercase")]
pub enum Arg {
    Text(String),
    Num(u64),
    Size(u64),
    /// A duration in seconds.
    Age(i64),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Msg {
    /// `alert.<kind>` in the catalogs.
    pub kind: String,
    pub args: Vec<(String, Arg)>,
}

impl Msg {
    pub fn new(kind: &str) -> Msg {
        Msg {
            kind: kind.to_string(),
            args: Vec::new(),
        }
    }

    pub fn text(mut self, name: &str, v: impl Into<String>) -> Msg {
        self.args.push((name.into(), Arg::Text(v.into())));
        self
    }

    pub fn num(mut self, name: &str, v: u64) -> Msg {
        self.args.push((name.into(), Arg::Num(v)));
        self
    }

    pub fn size(mut self, name: &str, v: u64) -> Msg {
        self.args.push((name.into(), Arg::Size(v)));
        self
    }

    pub fn age(mut self, name: &str, secs: i64) -> Msg {
        self.args.push((name.into(), Arg::Age(secs)));
        self
    }

    /// The message in `lang`.
    pub fn render(&self, i18n: &I18n, lang: Lang) -> String {
        let args: Vec<(&str, String)> = self
            .args
            .iter()
            .map(|(k, v)| {
                let s = match v {
                    Arg::Text(t) => t.clone(),
                    Arg::Num(n) => i18n.number(lang, *n),
                    Arg::Size(b) => i18n.size(lang, *b),
                    Arg::Age(secs) => age(i18n, lang, *secs),
                };
                (k.as_str(), s)
            })
            .collect();
        let refs: Vec<(&str, &str)> = args.iter().map(|(k, v)| (*k, v.as_str())).collect();
        i18n.tf(lang, &format!("alert.{}", self.kind), &refs)
    }

    /// The message for an alert that went away: `alert.<kind>_ok` if the catalog has one,
    /// else "Resolved: <message>".
    pub fn render_resolved(&self, i18n: &I18n, lang: Lang) -> String {
        let ok = Msg {
            kind: format!("{}_ok", self.kind),
            args: self.args.clone(),
        };
        if i18n.has(lang, &format!("alert.{}", ok.kind)) {
            ok.render(i18n, lang)
        } else {
            i18n.tf(
                lang,
                "alert.resolved",
                &[("title", &self.render(i18n, lang))],
            )
        }
    }
}

/// "45 min", "3 h", "2 days".
pub fn age(i18n: &I18n, lang: Lang, secs: i64) -> String {
    let secs = secs.max(0) as u64;
    if secs < 2 * 3600 {
        i18n.tn(lang, "age.minutes", (secs / 60).max(1), &[])
    } else if secs < 2 * 86_400 {
        i18n.tn(lang, "age.hours", secs / 3600, &[])
    } else {
        i18n.tn(lang, "age.days", secs / 86_400, &[])
    }
}

/// Something wrong right now.
#[derive(Debug, Clone, PartialEq)]
pub struct Condition {
    pub key: String,
    pub severity: Severity,
    pub msg: Msg,
}

/// Something that happened once.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Event {
    pub key: String,
    pub severity: Severity,
    pub msg: Msg,
}

/// An alert that is firing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Active {
    pub key: String,
    pub severity: Severity,
    pub msg: Msg,
    pub since: i64,
    pub last_sent: i64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Baseline {
    pub value: u64,
    pub seen: i64,
}

/// What a check found.
#[derive(Debug, Default)]
pub struct Findings {
    pub conditions: Vec<Condition>,
    pub events: Vec<Event>,
    /// Key prefixes whose data is missing this time.
    pub unknown: Vec<String>,
}

/// What to tell the channels.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    Fired(Active),
    Escalated(Active),
    Repeated(Active),
    Resolved(Active),
    Event(Event),
}

/// One row of the alert history.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LogEntry {
    pub time: i64,
    pub key: String,
    /// fired, escalated, repeated, resolved, event
    pub what: String,
    pub severity: Severity,
    pub msg: Msg,
}

impl Change {
    pub fn log_entry(&self, time: i64) -> LogEntry {
        let (what, a_key, severity, msg) = match self {
            Change::Fired(a) => ("fired", &a.key, a.severity, &a.msg),
            Change::Escalated(a) => ("escalated", &a.key, a.severity, &a.msg),
            Change::Repeated(a) => ("repeated", &a.key, a.severity, &a.msg),
            Change::Resolved(a) => ("resolved", &a.key, a.severity, &a.msg),
            Change::Event(e) => ("event", &e.key, e.severity, &e.msg),
        };
        LogEntry {
            time,
            key: a_key.clone(),
            what: what.into(),
            severity,
            msg: msg.clone(),
        }
    }
}

// ---------------------------------------------------------------------------------------
// Rules

struct Ctx<'a> {
    t: Thresholds,
    cfg: &'a AlertsConfig,
    active: &'a BTreeMap<String, Active>,
    baselines: &'a mut BTreeMap<String, Baseline>,
    now: i64,
    out: Findings,
}

impl Ctx<'_> {
    fn is_active(&self, key: &str) -> bool {
        self.active.contains_key(key)
    }

    /// Above `limit`, or above `limit - hysteresis` while the alert is active.
    fn above(&self, key: &str, value: i64, limit: i64, hysteresis: i64) -> bool {
        value > limit || (self.is_active(key) && value > limit - hysteresis)
    }

    fn cond(&mut self, key: String, severity: Severity, msg: Msg) {
        self.out.conditions.push(Condition { key, severity, msg });
    }

    fn unknown(&mut self, prefix: impl Into<String>) {
        self.out.unknown.push(prefix.into());
    }

    /// Compare a counter with its baseline; returns the old value if it rose. `seen` moves
    /// on once a day while the value stays, so an unchanged state needs no write.
    fn rose(&mut self, key: String, value: u64) -> Option<u64> {
        let now = self.now;
        let prev = self.baselines.get(&key).copied();
        let seen = match prev {
            Some(b) if b.value == value && now - b.seen < 86_400 => b.seen,
            _ => now,
        };
        self.baselines.insert(key, Baseline { value, seen });
        prev.map(|b| b.value).filter(|old| value > *old)
    }

    fn event(&mut self, key: String, severity: Severity, msg: Msg) {
        self.out.events.push(Event { key, severity, msg });
    }
}

fn disk_name(snap: &Snapshot, dev: Option<&str>, fallback: &str) -> String {
    dev.and_then(|d| snap.disk(d))
        .map(Disk::display_name)
        .unwrap_or_else(|| fallback.to_string())
}

fn pool_rules(c: &mut Ctx, snap: &Snapshot) {
    let pools = match &snap.pools {
        Probe::Ok(p) => p,
        Probe::Absent => return,
        Probe::Failed(e) => {
            c.unknown("pool:");
            c.cond(
                "probe:zpool".into(),
                Severity::Warning,
                Msg::new("probe_failed")
                    .text("what", "zpool")
                    .text("error", e.clone()),
            );
            return;
        }
    };
    for p in pools {
        let k = |s: &str| format!("pool:{}:{s}", p.name);
        if p.state != "ONLINE" {
            let problems: Vec<String> = p
                .all_vdevs()
                .into_iter()
                .filter(|v| {
                    v.is_leaf() && !matches!(v.state.as_str(), "ONLINE" | "AVAIL" | "INUSE")
                })
                .map(|v| {
                    format!(
                        "{} {}",
                        disk_name(snap, v.disk.as_deref(), &v.name),
                        v.state
                    )
                })
                .collect();
            let msg = if problems.is_empty() {
                Msg::new("pool_state")
            } else {
                Msg::new("pool_state_devices").text("devices", problems.join(", "))
            };
            c.cond(
                k("state"),
                Severity::Critical,
                msg.text("pool", &p.name).text("state", &p.state),
            );
        }
        if p.data_errors > 0 {
            c.cond(
                k("data_errors"),
                Severity::Critical,
                Msg::new("pool_data_errors")
                    .text("pool", &p.name)
                    .num("count", p.data_errors),
            );
        }
        let cap = p.capacity as i64;
        let full = k("full");
        let was = c.active.get(&full).map(|a| a.severity);
        let (warn, crit) = (i64::from(c.t.pool_warn), i64::from(c.t.pool_crit));
        let sev = if cap > crit || (was == Some(Severity::Critical) && cap > crit - 2) {
            Some(Severity::Critical)
        } else if cap > warn || (was.is_some() && cap > warn - 2) {
            Some(Severity::Warning)
        } else {
            None
        };
        if let Some(sev) = sev {
            c.cond(
                full,
                sev,
                Msg::new("pool_full")
                    .text("pool", &p.name)
                    .num("percent", p.capacity)
                    .size("free", p.free),
            );
        }
        if let Some(scan) = &p.scan {
            if scan.is_resilver() && scan.running() {
                c.cond(
                    k("resilver"),
                    Severity::Warning,
                    Msg::new("pool_resilver")
                        .text("pool", &p.name)
                        .num("percent", scan.percent() as u64),
                );
            }
            if scan.is_scrub() && !scan.running() {
                if scan.errors > 0 {
                    c.cond(
                        k("scrub_errors"),
                        Severity::Warning,
                        Msg::new("pool_scrub_errors")
                            .text("pool", &p.name)
                            .num("count", scan.errors),
                    );
                }
                let last = scan.end.unwrap_or(scan.start);
                let age = c.now - last;
                if last > 0 && age > i64::from(c.t.scrub_days) * 86_400 {
                    c.cond(
                        k("scrub_old"),
                        Severity::Warning,
                        Msg::new("pool_scrub_old")
                            .text("pool", &p.name)
                            .age("age", age),
                    );
                }
            }
        }
        for v in p.all_vdevs() {
            let name = disk_name(snap, v.disk.as_deref(), &v.name);
            for (counter, value) in [("read", v.read), ("write", v.write), ("cksum", v.cksum)] {
                let key = format!("pool:{}:{}:{counter}", p.name, v.guid);
                if let Some(old) = c.rose(key.clone(), value) {
                    c.event(
                        key,
                        Severity::Warning,
                        Msg::new(&format!("vdev_{counter}"))
                            .text("pool", &p.name)
                            .text("device", name.clone())
                            .num("old", old)
                            .num("new", value),
                    );
                }
            }
        }
    }
}

fn dataset_rules(c: &mut Ctx, snap: &Snapshot) {
    let datasets = match &snap.datasets {
        Probe::Ok(d) => d,
        Probe::Absent => return,
        Probe::Failed(e) => {
            c.unknown("ds:");
            c.cond(
                "probe:zfs".into(),
                Severity::Warning,
                Msg::new("probe_failed")
                    .text("what", "zfs")
                    .text("error", e.clone()),
            );
            return;
        }
    };
    for d in datasets.iter().map(|i| &i.ds) {
        if let Some(pct) = d.quota_percent() {
            let key = format!("ds:{}:quota", d.name);
            if c.above(&key, pct as i64, c.t.quota.into(), 2) {
                c.cond(
                    key,
                    Severity::Warning,
                    Msg::new("ds_quota")
                        .text("dataset", &d.name)
                        .num("percent", pct),
                );
            }
        }
        if let Some(limit) = c.cfg.snapshot_limit(&d.name) {
            let key = format!("ds:{}:snapshots", d.name);
            match d.newest_snapshot {
                None => c.cond(
                    key,
                    Severity::Warning,
                    Msg::new("ds_no_snapshots").text("dataset", &d.name),
                ),
                Some(t) if c.now - t > limit => c.cond(
                    key,
                    Severity::Warning,
                    Msg::new("ds_snapshot_old")
                        .text("dataset", &d.name)
                        .age("age", c.now - t),
                ),
                Some(_) => {}
            }
        }
    }
}

fn md_rules(c: &mut Ctx, snap: &Snapshot) {
    let arrays = match &snap.md {
        Probe::Ok(a) => a,
        Probe::Absent => return,
        Probe::Failed(e) => {
            c.unknown("md:");
            c.cond(
                "probe:mdstat".into(),
                Severity::Warning,
                Msg::new("probe_failed")
                    .text("what", "/proc/mdstat")
                    .text("error", e.clone()),
            );
            return;
        }
    };
    for a in arrays {
        if a.inactive() {
            c.cond(
                format!("md:{}:inactive", a.name),
                Severity::Warning,
                Msg::new("md_inactive").text("array", &a.name),
            );
        } else if a.degraded() {
            let failed: Vec<String> = a
                .members
                .iter()
                .filter(|m| m.faulty)
                .map(|m| format!("{} ({})", disk_name(snap, m.disk.as_deref(), &m.dev), m.dev))
                .collect();
            let msg = match &a.sync {
                Some(s) => Msg::new("md_degraded_sync")
                    .text("action", &s.action)
                    .num("percent", s.percent as u64),
                None => Msg::new("md_degraded"),
            };
            let detail = if failed.is_empty() {
                a.status.clone()
            } else {
                format!("{} – {}", a.status, failed.join(", "))
            };
            c.cond(
                format!("md:{}:degraded", a.name),
                Severity::Critical,
                msg.text("array", &a.name).text("status", detail),
            );
        }
    }
}

fn disk_rules(c: &mut Ctx, snap: &Snapshot) {
    match &snap.smart {
        SmartFile::Missing => {
            c.unknown("disk:");
            if !snap.disks.is_empty() {
                c.cond(
                    "smart:missing".into(),
                    Severity::Warning,
                    Msg::new("smart_missing"),
                );
            }
            return;
        }
        SmartFile::Failed { message } => {
            c.unknown("disk:");
            c.cond(
                "smart:failed".into(),
                Severity::Warning,
                Msg::new("smart_failed").text("error", message.clone()),
            );
            return;
        }
        SmartFile::Ok { collected } => {
            let age = c.now - collected;
            if age > i64::from(c.t.stale_minutes) * 60 {
                c.unknown("disk:");
                c.cond(
                    "smart:stale".into(),
                    Severity::Warning,
                    Msg::new("smart_stale").age("age", age),
                );
                return;
            }
        }
    }
    for d in &snap.disks {
        let id = d.id();
        let Some(s) = &d.smart else {
            c.unknown(format!("disk:{id}:"));
            continue;
        };
        let name = d.display_name();
        let k = |what: &str| format!("disk:{id}:{what}");
        if s.passed == Some(false) {
            c.cond(
                k("smart"),
                Severity::Critical,
                Msg::new("disk_smart_failed").text("disk", name.clone()),
            );
        }
        if let Some(temp) = s.temp {
            let limit = if d.kind == Kind::Hdd {
                c.t.hdd_temp
            } else {
                c.t.ssd_temp
            };
            let key = k("temp");
            if c.above(&key, temp.into(), limit.into(), 3) {
                c.cond(
                    key,
                    Severity::Warning,
                    Msg::new("disk_temp")
                        .text("disk", name.clone())
                        .num("temp", temp.max(0) as u64)
                        .num("limit", limit.max(0) as u64),
                );
            }
        }
        if let Some(wear) = s.wear
            && wear > c.t.ssd_wear.into()
        {
            c.cond(
                k("wear"),
                Severity::Warning,
                Msg::new("disk_wear")
                    .text("disk", name.clone())
                    .num("percent", wear),
            );
        }
        if let Some(code) = s.nvme_critical_warning.filter(|w| *w != 0) {
            c.cond(
                k("nvme_warning"),
                Severity::Critical,
                Msg::new("disk_nvme_warning")
                    .text("disk", name.clone())
                    .text("code", format!("0x{code:02x}")),
            );
        }
        if let Some(t) = s.self_test.as_ref().filter(|t| t.passed == Some(false)) {
            c.cond(
                k("selftest"),
                Severity::Critical,
                Msg::new("disk_selftest_failed")
                    .text("disk", name.clone())
                    .text("result", &t.result),
            );
        }
        for (counter, value) in [
            ("realloc", s.realloc),
            ("pending", s.pending),
            ("uncorrectable", s.uncorrectable),
            ("crc", s.crc),
            ("media_errors", s.media_errors),
        ] {
            let Some(value) = value else { continue };
            let key = k(counter);
            if let Some(old) = c.rose(key.clone(), value) {
                c.event(
                    key,
                    Severity::Warning,
                    Msg::new(&format!("disk_{counter}"))
                        .text("disk", name.clone())
                        .num("old", old)
                        .num("new", value),
                );
            }
        }
    }
}

fn host_rules(c: &mut Ctx, snap: &Snapshot) {
    match snap.host.root_fs {
        Some(fs) => {
            let key = "host:rootfs".to_string();
            if c.above(&key, fs.percent() as i64, c.t.root_fs.into(), 1) {
                c.cond(
                    key,
                    Severity::Warning,
                    Msg::new("rootfs_full")
                        .num("percent", fs.percent())
                        .size("free", fs.avail),
                );
            }
        }
        None => c.unknown("host:"),
    }
}

fn app_rules(c: &mut Ctx, snap: &Snapshot) {
    for (user, role) in &snap.app.missing_roles {
        c.cond(
            format!("user:{}:role", user.to_lowercase()),
            Severity::Warning,
            Msg::new("user_role_missing")
                .text("user", user)
                .text("role", role),
        );
    }
    if let Some(e) = snap.app.backup.as_ref().and_then(|b| b.error.as_ref()) {
        c.cond(
            "backup:failed".to_string(),
            Severity::Warning,
            Msg::new("backup_failed").text("error", e),
        );
    }
    for s in &snap.app.missing_shares {
        c.cond(
            format!("share:{}:missing", s.id),
            Severity::Warning,
            Msg::new("share_missing")
                .text("id", &s.id)
                .text("owner", &s.owner)
                .text("path", format!("{}/{}", s.volume, s.path)),
        );
    }
}

/// Run every rule over `snap`. `baselines` (counter values seen so far) is updated.
pub fn evaluate(
    snap: &Snapshot,
    cfg: &AlertsConfig,
    active: &BTreeMap<String, Active>,
    baselines: &mut BTreeMap<String, Baseline>,
    now: i64,
) -> Findings {
    let mut c = Ctx {
        t: cfg.thresholds,
        cfg,
        active,
        baselines,
        now,
        out: Findings::default(),
    };
    pool_rules(&mut c, snap);
    dataset_rules(&mut c, snap);
    md_rules(&mut c, snap);
    disk_rules(&mut c, snap);
    host_rules(&mut c, snap);
    app_rules(&mut c, snap);
    let out = c.out;
    baselines.retain(|_, b| now - b.seen < BASELINE_TTL);
    out
}

/// Apply findings to the active alerts; returns what to send and log.
pub fn step(active: &mut BTreeMap<String, Active>, findings: Findings, now: i64) -> Vec<Change> {
    let mut changes = Vec::new();
    let mut seen = BTreeSet::new();
    for c in findings.conditions {
        seen.insert(c.key.clone());
        match active.get_mut(&c.key) {
            None => {
                let a = Active {
                    key: c.key.clone(),
                    severity: c.severity,
                    msg: c.msg,
                    since: now,
                    last_sent: now,
                };
                active.insert(c.key, a.clone());
                changes.push(Change::Fired(a));
            }
            Some(a) => {
                let escalated = c.severity > a.severity;
                a.severity = c.severity;
                a.msg = c.msg;
                if escalated {
                    a.last_sent = now;
                    changes.push(Change::Escalated(a.clone()));
                } else if a.severity == Severity::Critical && now - a.last_sent >= REPEAT_SECS {
                    a.last_sent = now;
                    changes.push(Change::Repeated(a.clone()));
                }
            }
        }
    }
    let gone: Vec<String> = active
        .keys()
        .filter(|k| {
            !seen.contains(*k) && !findings.unknown.iter().any(|p| k.starts_with(p.as_str()))
        })
        .cloned()
        .collect();
    for k in gone {
        if let Some(a) = active.remove(&k) {
            changes.push(Change::Resolved(a));
        }
    }
    changes.extend(findings.events.into_iter().map(Change::Event));
    changes
}

/// The dashboard's one-word answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Ok,
    Warning,
    Critical,
    /// Monitoring itself is behind or broken.
    Stale,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Ok => "ok",
            Verdict::Warning => "warning",
            Verdict::Critical => "critical",
            Verdict::Stale => "stale",
        }
    }
}

/// Critical beats stale beats warning. `snapshot_age` is how old the newest reading is.
pub fn verdict<'a>(
    active: impl IntoIterator<Item = &'a Active>,
    snapshot_age: Option<i64>,
) -> Verdict {
    let mut worst = None;
    let mut stale = snapshot_age.is_none_or(|a| a > 5 * 60);
    for a in active {
        if a.key.starts_with("smart:") || a.key.starts_with("probe:") {
            stale = true;
        }
        worst = worst.max(Some(a.severity));
    }
    match (worst, stale) {
        (Some(Severity::Critical), _) => Verdict::Critical,
        (_, true) => Verdict::Stale,
        (Some(Severity::Warning), _) => Verdict::Warning,
        _ => Verdict::Ok,
    }
}

/// One-shot events raised by the web side (sign-ins, shares, public uploads), with a
/// per-key cooldown so a burst sends one message.
#[derive(Default)]
pub struct Cooldowns {
    last: std::sync::Mutex<HashMap<String, i64>>,
}

impl Cooldowns {
    /// True if `key` may fire now (and remembers it).
    pub fn allow(&self, key: &str, cooldown: i64, now: i64) -> bool {
        let mut m = self.last.lock().unwrap();
        if m.get(key).is_some_and(|t| now - t < cooldown) {
            return false;
        }
        if m.len() > 10_000 {
            m.retain(|_, t| now - *t < 86_400);
        }
        m.insert(key.to_string(), now);
        true
    }
}

#[cfg(test)]
mod tests;
