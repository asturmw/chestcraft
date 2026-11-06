//! The health monitor: a snapshot of the machine every minute, alert rules over it, and
//! messages to the alert channels; one reading per hour for the charts; the weekly digest.
//! Events from requests (an admin signing in from a new address, a share slowed down by
//! wrong passwords, public uploads) go out through [`raise`].

use super::Shared;
use crate::config::WeeklyTime;
use crate::health::alerts::{
    self, Active, Baseline, Change, Cooldowns, Event, LogEntry, Msg, Severity,
};
use crate::health::history::{self, Sample};
use crate::health::{self, Roots, Snapshot};
use crate::i18n::Lang;
use crate::notify::{Level, Message};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

const INTERVAL: Duration = Duration::from_secs(60);
const LOG_KEEP_SECS: i64 = 365 * 86_400;

pub struct Monitor {
    latest: RwLock<Option<Arc<Snapshot>>>,
    active: RwLock<BTreeMap<String, Active>>,
    history: RwLock<Arc<Vec<(i64, Sample)>>>,
    arc_prev: Mutex<Option<(u64, u64)>>,
    last_hour: Mutex<i64>,
    pub cooldowns: Cooldowns,
    pub roots: Roots,
}

impl Default for Monitor {
    fn default() -> Self {
        Monitor {
            latest: RwLock::new(None),
            active: RwLock::new(BTreeMap::new()),
            history: RwLock::new(Arc::new(Vec::new())),
            arc_prev: Mutex::new(None),
            last_hour: Mutex::new(0),
            cooldowns: Cooldowns::default(),
            roots: Roots::default(),
        }
    }
}

impl Monitor {
    pub fn latest(&self) -> Option<Arc<Snapshot>> {
        self.latest.read().unwrap().clone()
    }

    pub fn active(&self) -> Vec<Active> {
        let mut v: Vec<Active> = self.active.read().unwrap().values().cloned().collect();
        v.sort_by(|a, b| b.severity.cmp(&a.severity).then(a.since.cmp(&b.since)));
        v
    }

    pub fn history(&self) -> Arc<Vec<(i64, Sample)>> {
        self.history.read().unwrap().clone()
    }

    /// The dashboard's verdict right now.
    pub fn verdict(&self, now: i64) -> alerts::Verdict {
        let age = self.latest().map(|s| now - s.taken);
        alerts::verdict(self.active.read().unwrap().values(), age)
    }

    /// Put a snapshot in place without reading the machine (tests).
    pub fn set_latest(&self, s: Snapshot) {
        *self.latest.write().unwrap() = Some(Arc::new(s));
    }

    pub fn set_history(&self, h: Vec<(i64, Sample)>) {
        *self.history.write().unwrap() = Arc::new(h);
    }
}

/// The language alert messages are written in.
pub fn alert_lang(st: &super::AppState) -> Lang {
    st.i18n
        .by_code(&st.cfg.alerts.language)
        .unwrap_or(crate::i18n::EN)
}

fn level(s: Severity) -> Level {
    match s {
        Severity::Info => Level::Info,
        Severity::Warning => Level::Warning,
        Severity::Critical => Level::Critical,
    }
}

/// The footer of every message: when, and where to look.
fn footer(st: &super::AppState, lang: Lang, now: i64) -> String {
    format!(
        "chestcraft · {}\n{}/.cc/admin/dashboard",
        st.i18n.datetime(lang, now, &st.cfg.server.timezone),
        st.cfg.server.public_url
    )
}

/// What a change looks like in Telegram/ntfy.
pub fn message(st: &super::AppState, change: &Change, now: i64) -> Message {
    let lang = alert_lang(st);
    let i18n = &st.i18n;
    let (level, title, extra) = match change {
        Change::Fired(a) | Change::Escalated(a) => {
            (level(a.severity), a.msg.render(i18n, lang), None)
        }
        Change::Repeated(a) => (
            level(a.severity),
            a.msg.render(i18n, lang),
            Some(i18n.tf(
                lang,
                "alert.still_since",
                &[(
                    "since",
                    &i18n.datetime(lang, a.since, &st.cfg.server.timezone),
                )],
            )),
        ),
        Change::Resolved(a) => (Level::Resolved, a.msg.render_resolved(i18n, lang), None),
        Change::Event(e) => (level(e.severity), e.msg.render(i18n, lang), None),
    };
    let mut body = String::new();
    if let Some(x) = extra {
        body.push_str(&x);
        body.push('\n');
    }
    body.push_str(&footer(st, lang, now));
    Message { level, title, body }
}

/// Send a one-shot event (at most once per `cooldown` seconds for `key`) and log it.
pub fn raise(st: &super::AppState, key: &str, cooldown: i64, severity: Severity, msg: Msg) {
    let now = super::now();
    if !st.health.cooldowns.allow(key, cooldown, now) {
        return;
    }
    let change = Change::Event(Event {
        key: key.to_string(),
        severity,
        msg,
    });
    st.notifier.send(message(st, &change, now));
    let entry = change.log_entry(now);
    let db = st.db.clone();
    tokio::task::spawn_blocking(move || {
        if let Err(e) = db.log_alert(&entry) {
            tracing::warn!("can't log an alert: {e}");
        }
    });
}

/// Evaluate `snap`, store the outcome, and return what to send. Blocking.
pub fn apply(st: &super::AppState, snap: &Snapshot) -> Result<Vec<Change>, crate::db::DbError> {
    let now = snap.taken;
    let (mut active, mut baselines): (BTreeMap<String, Active>, BTreeMap<String, Baseline>) =
        st.db.alert_state()?;
    let (active0, baselines0) = (active.clone(), baselines.clone());
    let findings = alerts::evaluate(snap, &st.cfg.alerts, &active, &mut baselines, now);
    let changes = alerts::step(&mut active, findings, now);
    let log: Vec<LogEntry> = changes.iter().map(|c| c.log_entry(now)).collect();
    // Most minutes nothing changes: skip the (synced) write then.
    if !log.is_empty() || active != active0 || baselines != baselines0 {
        st.db.put_alert_state(&active, &baselines, &log)?;
    }
    *st.health.active.write().unwrap() = active;

    let hour = history::hour_of(now);
    let mut last = st.health.last_hour.lock().unwrap();
    if *last != hour {
        let sample = Sample::from_snapshot(snap);
        if !sample.is_empty() {
            st.db.put_history(hour, &sample)?;
            let mut h: Vec<(i64, Sample)> = st.health.history().as_ref().clone();
            h.retain(|(t, _)| *t != hour && now - t < history::KEEP_SECS);
            h.push((hour, sample));
            st.health.set_history(h);
        }
        st.db
            .prune_health(now - LOG_KEEP_SECS, now - history::KEEP_SECS)?;
        *last = hour;
    }
    Ok(changes)
}

/// One round: read the machine, evaluate, notify.
pub async fn tick(st: &Shared) {
    let arc_prev = *st.health.arc_prev.lock().unwrap();
    let snap = health::gather(
        &st.cfg,
        st.db.clone(),
        &st.volumes,
        &st.health.roots,
        arc_prev,
    )
    .await;
    if let Some(a) = snap.host.arc {
        *st.health.arc_prev.lock().unwrap() = Some((a.hits, a.misses));
    }
    let snap = Arc::new(snap);
    let (st2, s2) = (st.clone(), snap.clone());
    let changes = tokio::task::spawn_blocking(move || apply(&st2, &s2)).await;
    match changes {
        Ok(Ok(changes)) => {
            for c in &changes {
                st.notifier.send(message(st, c, snap.taken));
            }
        }
        Ok(Err(e)) => tracing::error!("can't store the alert state: {e}"),
        Err(_) => tracing::error!("evaluating alerts crashed"),
    }
    *st.health.latest.write().unwrap() = Some(snap);
}

/// Seconds from `now` until the next `at` (weekday + time) in `tz`.
pub fn secs_until_weekly(at: WeeklyTime, tz: &TimeZone, now: Timestamp) -> u64 {
    let mut date = now.to_zoned(tz.clone()).date();
    for _ in 0..9 {
        if date.weekday() == at.weekday
            && let Ok(t) = date
                .at(at.time.hour as i8, at.time.minute as i8, 0, 0)
                .to_zoned(tz.clone())
            && t.timestamp() > now
        {
            return (t.timestamp().as_second() - now.as_second()).max(1) as u64;
        }
        date = date.tomorrow().unwrap_or(date);
    }
    7 * 86_400
}

/// The weekly summary: proves the alert path works and gives the key numbers.
pub fn digest(
    st: &super::AppState,
    snap: Option<&Snapshot>,
    active: &[Active],
    now: i64,
) -> Message {
    let lang = alert_lang(st);
    let i = &st.i18n;
    let mut lines = Vec::new();
    if active.is_empty() {
        lines.push(i.t(lang, "digest.all_ok").to_string());
    } else {
        lines.push(i.tn(lang, "digest.active", active.len() as u64, &[]));
        for a in active.iter().take(10) {
            lines.push(format!("• {}", a.msg.render(i, lang)));
        }
    }
    if let Some(s) = snap {
        for p in s.pools.ok().into_iter().flatten() {
            let scrub = p
                .scan
                .as_ref()
                .filter(|sc| sc.is_scrub())
                .map(|sc| {
                    i.tf(
                        lang,
                        "digest.scrub",
                        &[(
                            "age",
                            &alerts::age(i, lang, now - sc.end.unwrap_or(sc.start)),
                        )],
                    )
                })
                .unwrap_or_default();
            lines.push(i.tf(
                lang,
                "digest.pool",
                &[
                    ("pool", &p.name),
                    ("state", &p.state),
                    ("percent", &p.capacity.to_string()),
                    ("free", &i.size(lang, p.free)),
                    ("scrub", &scrub),
                ],
            ));
        }
        for a in s.md.ok().into_iter().flatten() {
            lines.push(format!("{}: {} [{}]", a.name, a.level, a.status));
        }
        let temps: Vec<i32> = s
            .disks
            .iter()
            .filter_map(|d| d.smart.as_ref()?.temp)
            .collect();
        if !s.disks.is_empty() {
            lines.push(
                i.tf(
                    lang,
                    "digest.disks",
                    &[
                        ("count", &s.disks.len().to_string()),
                        (
                            "hottest",
                            &temps
                                .iter()
                                .max()
                                .map(|t| format!("{t} °C"))
                                .unwrap_or_else(|| "–".into()),
                        ),
                    ],
                ),
            );
        }
        if let Some(fs) = s.host.root_fs {
            lines.push(i.tf(
                lang,
                "digest.rootfs",
                &[("percent", &fs.percent().to_string())],
            ));
        }
    }
    lines.push(footer(st, lang, now));
    Message {
        level: if active.iter().any(|a| a.severity == Severity::Critical) {
            Level::Critical
        } else if active.is_empty() {
            Level::Info
        } else {
            Level::Warning
        },
        title: i.t(lang, "digest.title").to_string(),
        body: lines.join("\n"),
    }
}

pub fn spawn(st: Shared) {
    // Alerts already active keep their state across restarts.
    {
        let st = st.clone();
        tokio::spawn(async move {
            let since = super::now() - history::KEEP_SECS;
            let st2 = st.clone();
            let loaded = tokio::task::spawn_blocking(move || {
                let active = st2.db.alert_state::<Active, Baseline>().map(|(a, _)| a);
                let history = st2.db.history::<Sample>(since);
                (active, history)
            })
            .await;
            if let Ok((active, history)) = loaded {
                if let Ok(a) = active {
                    *st.health.active.write().unwrap() = a;
                }
                match history {
                    Ok(h) => st.health.set_history(h),
                    Err(e) => tracing::warn!("can't read the dashboard history: {e}"),
                }
            }
            loop {
                tick(&st).await;
                tokio::time::sleep(INTERVAL).await;
            }
        });
    }
    tokio::spawn(st.notifier.clone().run());
    tokio::spawn(async move {
        loop {
            let wait = secs_until_weekly(
                st.cfg.alerts.digest,
                &st.cfg.server.timezone,
                Timestamp::now(),
            );
            tokio::time::sleep(Duration::from_secs(wait)).await;
            let snap = st.health.latest();
            let msg = digest(&st, snap.as_deref(), &st.health.active(), super::now());
            st.notifier.send(msg);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TimeOfDay;
    use jiff::civil::Weekday;

    #[test]
    fn weekly_schedule() {
        let tz = TimeZone::get("Europe/Berlin").unwrap();
        let sun10 = WeeklyTime {
            weekday: Weekday::Sunday,
            time: TimeOfDay {
                hour: 10,
                minute: 0,
            },
        };
        // Friday 2026-11-06 12:00 CET → Sunday 10:00 CET: 46 hours.
        let now: Timestamp = "2026-11-06T11:00:00Z".parse().unwrap();
        assert_eq!(secs_until_weekly(sun10, &tz, now), 46 * 3600);
        // Sunday exactly at 10:00 → next week.
        let now: Timestamp = "2026-11-08T09:00:00Z".parse().unwrap();
        assert_eq!(secs_until_weekly(sun10, &tz, now), 7 * 86_400);
    }
}
