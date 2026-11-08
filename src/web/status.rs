//! `/healthz` and the status API.
//!
//! `/healthz` needs no sign-in and answers from memory: `200 ok` while chestcraft and its
//! monitoring work, else `503` with the problem words ([`Problem`]). The machine's own
//! trouble (a degraded pool, a hot disk) is not a 503: chestcraft reports that itself.
//!
//! `/api/v1/status` is JSON for a watchdog or monitoring tool: the same verdict and alerts
//! as the dashboard plus a summary of pools, datasets, md arrays, disks, host and the alert
//! channels. LAN/Tailscale only; `Authorization: Bearer ccs_…` with a token from the
//! volumes page. Wrong tokens slow the address down like wrong passwords. The format is
//! documented in `docs/api.md`; fields are only ever added within `api: 1`.

use super::monitor::{Problem, alert_lang};
use super::{ClientInfo, Shared, audit, now};
use crate::auth::apitoken;
use crate::auth::perm::Origin;
use crate::health::alerts::{self, Severity, Verdict};
use crate::health::disks::{Kind, Role, SmartState};
use crate::health::{Probe, SmartFile, Snapshot};
use axum::Json;
use axum::extract::{Extension, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use std::collections::BTreeMap;

pub const API_VERSION: u32 = 1;
/// Backoff scope for wrong tokens (no account name contains a colon).
const BACKOFF_SCOPE: &str = "api:status";

pub async fn healthz(State(st): State<Shared>) -> Response {
    let problems = st.health.problems(now());
    let (status, body) = if problems.is_empty() {
        (StatusCode::OK, "ok\n".to_string())
    } else {
        let words: Vec<&str> = problems.iter().map(|p| p.as_str()).collect();
        (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("{}\n", words.join(" ")),
        )
    };
    (
        status,
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response()
}

fn json_error(status: StatusCode, error: &str) -> Response {
    let mut res = (status, Json(serde_json::json!({ "error": error }))).into_response();
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

/// The token from `Authorization: Bearer …`.
fn bearer(headers: &HeaderMap) -> Option<&str> {
    let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?.trim();
    let (scheme, token) = v.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| token.trim())
        .filter(|t| !t.is_empty())
}

pub async fn status(
    State(st): State<Shared>,
    Extension(client): Extension<ClientInfo>,
    headers: HeaderMap,
) -> Response {
    // From the internet the API doesn't exist.
    if client.origin == Origin::Remote {
        return (StatusCode::NOT_FOUND, "Not found\n").into_response();
    }
    let now = now();
    let wait = st.backoff.wait(client.ip, BACKOFF_SCOPE, now);
    if wait > 0 {
        let mut res = json_error(StatusCode::TOO_MANY_REQUESTS, "too many wrong tokens");
        if let Ok(v) = HeaderValue::from_str(&wait.to_string()) {
            res.headers_mut().insert(header::RETRY_AFTER, v);
        }
        return res;
    }
    let unauthorized = |error: &str| {
        let mut res = json_error(StatusCode::UNAUTHORIZED, error);
        res.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Bearer realm=\"chestcraft\""),
        );
        res
    };
    let Some(token) = bearer(&headers) else {
        return unauthorized("token required");
    };
    let verified = {
        let (st, token, ip) = (st.clone(), token.to_string(), client.ip.to_string());
        tokio::task::spawn_blocking(move || {
            let (hash, rec) = apitoken::verify(&st.db, &token).ok().flatten()?;
            apitoken::touch(&st.db, &hash, &rec, &ip, now);
            Some(rec)
        })
        .await
        .ok()
        .flatten()
    };
    if verified.is_none() {
        st.backoff.fail(client.ip, BACKOFF_SCOPE, true, now);
        audit(&st, "anon".into(), client.ip, "api_token_failed", "", "");
        return unauthorized("invalid token");
    }
    st.backoff.succeed(client.ip, BACKOFF_SCOPE);
    let channels = {
        let st = st.clone();
        tokio::task::spawn_blocking(move || channels(&st))
            .await
            .unwrap_or_default()
    };
    let body = build(&st, st.health.latest().as_deref(), channels, now);
    let mut res = Json(body).into_response();
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

// ---------------------------------------------------------------------------------------
// The response (api 1)

#[derive(Debug, Serialize)]
pub struct Status {
    pub api: u32,
    pub version: &'static str,
    pub now: i64,
    pub started: i64,
    /// Same as `/healthz` answering 200.
    pub healthy: bool,
    pub problems: Vec<Problem>,
    pub verdict: Verdict,
    /// When the newest snapshot was taken; null before the first one.
    pub as_of: Option<i64>,
    /// State of each health source in the snapshot.
    pub sources: BTreeMap<&'static str, Source>,
    pub alerts: Vec<AlertOut>,
    pub pools: Vec<PoolOut>,
    pub datasets: Vec<DatasetOut>,
    pub md: Vec<MdOut>,
    pub disks: Vec<DiskOut>,
    pub host: Option<HostOut>,
    pub app: Option<AppOut>,
    pub channels: Vec<ChannelOut>,
}

#[derive(Debug, Serialize)]
pub struct Source {
    /// ok, absent, failed; SMART also missing or stale.
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// SMART: when the collector last ran.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collected: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct AlertOut {
    pub key: String,
    pub severity: Severity,
    /// In the alert language from the config.
    pub title: String,
    pub since: i64,
}

#[derive(Debug, Serialize)]
pub struct PoolOut {
    pub name: String,
    pub state: String,
    pub size: u64,
    pub alloc: u64,
    pub free: u64,
    pub capacity: u64,
    pub frag: Option<u64>,
    /// Error counters summed over every vdev.
    pub read_errors: u64,
    pub write_errors: u64,
    pub cksum_errors: u64,
    /// Files with permanent errors.
    pub data_errors: u64,
    /// Vdevs (any level) that aren't ONLINE.
    pub vdevs_not_online: Vec<String>,
    pub scan: Option<ScanOut>,
}

#[derive(Debug, Serialize)]
pub struct ScanOut {
    pub function: String,
    pub state: String,
    pub start: i64,
    pub end: Option<i64>,
    pub errors: u64,
    pub percent: f64,
    pub paused: bool,
}

#[derive(Debug, Serialize)]
pub struct DatasetOut {
    pub name: String,
    pub pool: String,
    pub used: u64,
    pub avail: u64,
    /// 0 = none. Limits `used`.
    pub quota: u64,
    /// 0 = none. Limits `referenced`.
    pub refquota: u64,
    /// The dataset's own data, without snapshots and children.
    pub referenced: u64,
    pub snapshots: u64,
    pub snap_used: u64,
    pub newest_snapshot: Option<i64>,
    /// chestcraft volumes on this dataset.
    pub volumes: Vec<String>,
    /// Bytes in chestcraft's trash on this dataset.
    pub trash: u64,
}

#[derive(Debug, Serialize)]
pub struct MdOut {
    pub name: String,
    pub state: String,
    pub level: String,
    /// `[UU_]` as in /proc/mdstat.
    pub status: String,
    pub degraded: bool,
    pub members: Vec<String>,
    pub faulty: Vec<String>,
    pub sync: Option<SyncOut>,
}

#[derive(Debug, Serialize)]
pub struct SyncOut {
    pub action: String,
    pub percent: f64,
}

#[derive(Debug, Serialize)]
pub struct DiskOut {
    pub name: String,
    pub label: Option<String>,
    /// hdd, ssd, nvme.
    pub kind: &'static str,
    pub model: String,
    pub serial: Option<String>,
    pub size: u64,
    pub pools: Vec<String>,
    pub md: Vec<String>,
    /// ok, standby (data from the last time it was awake), error, unsupported, none (the
    /// collector didn't report it).
    pub smart: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub smart_error: Option<String>,
    /// When the SMART data was read.
    pub checked: Option<i64>,
    pub passed: Option<bool>,
    pub temp: Option<i32>,
    pub power_on_hours: Option<u64>,
    pub realloc: Option<u64>,
    pub pending: Option<u64>,
    pub uncorrectable: Option<u64>,
    pub crc: Option<u64>,
    /// Percent of rated endurance used.
    pub wear: Option<u64>,
    pub media_errors: Option<u64>,
    pub self_test: Option<SelfTestOut>,
}

#[derive(Debug, Serialize)]
pub struct SelfTestOut {
    pub kind: String,
    pub passed: Option<bool>,
    pub result: String,
}

#[derive(Debug, Serialize)]
pub struct HostOut {
    pub uptime: Option<u64>,
    pub load: Option<[f64; 3]>,
    pub cpus: Option<usize>,
    pub mem_total: Option<u64>,
    pub mem_available: Option<u64>,
    pub cpu_temp: Option<f64>,
    pub arc_size: Option<u64>,
    pub arc_hit_rate: Option<f64>,
    pub root_fs: Option<FsOut>,
}

#[derive(Debug, Serialize)]
pub struct FsOut {
    pub total: u64,
    pub used: u64,
    pub avail: u64,
    pub percent: u64,
}

#[derive(Debug, Serialize)]
pub struct AppOut {
    pub sessions: usize,
    pub transfers: usize,
    pub shares_active: usize,
    pub failed_logins_24h: usize,
    /// MCP tool calls by agents in the last 24 hours.
    pub agent_calls_24h: usize,
    /// The nightly state backup; null until the first one ran.
    pub backup: Option<BackupOut>,
}

#[derive(Debug, Serialize)]
pub struct BackupOut {
    /// When the last attempt ended.
    pub attempt: i64,
    /// When the last good copy was made.
    pub last_ok: Option<i64>,
    /// Why the last attempt failed; null when it worked.
    pub error: Option<String>,
    pub bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct ChannelOut {
    /// telegram, ntfy.
    pub kind: &'static str,
    /// Configured and usable (token present).
    pub on: bool,
    /// Why it is off.
    pub problem: Option<String>,
    pub last_ok: Option<i64>,
    pub last_error: Option<String>,
    pub last_error_at: Option<i64>,
}

/// Configured channels with their delivery status (reads the database).
fn channels(st: &super::AppState) -> Vec<ChannelOut> {
    st.notifier
        .channels()
        .iter()
        .map(|c| {
            let s = st.notifier.status(c.kind);
            ChannelOut {
                kind: c.kind.name(),
                on: c.problem.is_none(),
                problem: c.problem.clone(),
                last_ok: s.last_ok,
                last_error: s.last_error,
                last_error_at: s.last_error_at,
            }
        })
        .collect()
}

fn probe<T>(p: &Probe<T>) -> Source {
    let (state, error) = match p {
        Probe::Absent => ("absent", None),
        Probe::Ok(_) => ("ok", None),
        Probe::Failed(e) => ("failed", Some(e.clone())),
    };
    Source {
        state,
        error,
        collected: None,
    }
}

fn kind(k: Kind) -> &'static str {
    match k {
        Kind::Hdd => "hdd",
        Kind::Ssd => "ssd",
        Kind::Nvme => "nvme",
    }
}

/// The status document for `snap` (None before the first snapshot).
pub fn build(
    st: &super::AppState,
    snap: Option<&Snapshot>,
    channels: Vec<ChannelOut>,
    now: i64,
) -> Status {
    let problems = st.health.problems(now);
    let lang = alert_lang(st);
    let active = st.health.active();
    let as_of = snap.map(|s| s.taken);
    let mut out = Status {
        api: API_VERSION,
        version: crate::VERSION,
        now,
        started: st.health.started(),
        healthy: problems.is_empty(),
        problems,
        verdict: alerts::verdict(&active, as_of.map(|t| now - t)),
        as_of,
        sources: BTreeMap::new(),
        alerts: active
            .iter()
            .map(|a| AlertOut {
                key: a.key.clone(),
                severity: a.severity,
                title: a.msg.render(&st.i18n, lang),
                since: a.since,
            })
            .collect(),
        pools: Vec::new(),
        datasets: Vec::new(),
        md: Vec::new(),
        disks: Vec::new(),
        host: None,
        app: None,
        channels,
    };
    let Some(s) = snap else {
        return out;
    };
    out.sources.insert("pools", probe(&s.pools));
    out.sources.insert("datasets", probe(&s.datasets));
    out.sources.insert("md", probe(&s.md));
    let stale_after = i64::from(st.cfg.alerts.thresholds.stale_minutes) * 60;
    out.sources.insert(
        "smart",
        match &s.smart {
            SmartFile::Missing => Source {
                state: "missing",
                error: None,
                collected: None,
            },
            SmartFile::Failed { message } => Source {
                state: "failed",
                error: Some(message.clone()),
                collected: None,
            },
            SmartFile::Ok { collected } => Source {
                state: if s.taken - collected > stale_after {
                    "stale"
                } else {
                    "ok"
                },
                error: None,
                collected: Some(*collected),
            },
        },
    );
    out.pools = s
        .pools
        .ok()
        .into_iter()
        .flatten()
        .map(|p| {
            let all = p.all_vdevs();
            PoolOut {
                name: p.name.clone(),
                state: p.state.clone(),
                size: p.size,
                alloc: p.alloc,
                free: p.free,
                capacity: p.capacity,
                frag: p.frag,
                read_errors: all.iter().map(|v| v.read).sum(),
                write_errors: all.iter().map(|v| v.write).sum(),
                cksum_errors: all.iter().map(|v| v.cksum).sum(),
                data_errors: p.data_errors,
                vdevs_not_online: all
                    .iter()
                    // Spares wait as AVAIL; that's their healthy state.
                    .filter(|v| v.state != "ONLINE" && v.state != "AVAIL")
                    .map(|v| v.name.clone())
                    .collect(),
                scan: p.scan.as_ref().map(|sc| ScanOut {
                    function: sc.function.clone(),
                    state: sc.state.clone(),
                    start: sc.start,
                    end: sc.end,
                    errors: sc.errors,
                    percent: sc.percent(),
                    paused: sc.paused,
                }),
            }
        })
        .collect();
    out.datasets = s
        .datasets
        .ok()
        .into_iter()
        .flatten()
        .map(|d| DatasetOut {
            name: d.ds.name.clone(),
            pool: d.ds.pool.clone(),
            used: d.ds.used,
            avail: d.ds.avail,
            quota: d.ds.quota,
            refquota: d.ds.refquota,
            referenced: d.ds.referenced,
            snapshots: d.ds.snapshots,
            snap_used: d.ds.snap_used,
            newest_snapshot: d.ds.newest_snapshot,
            volumes: d.volumes.clone(),
            trash: d.trash,
        })
        .collect();
    out.md =
        s.md.ok()
            .into_iter()
            .flatten()
            .map(|a| MdOut {
                name: a.name.clone(),
                state: a.state.clone(),
                level: a.level.clone(),
                status: a.status.clone(),
                degraded: !a.inactive() && a.degraded(),
                members: a.members.iter().map(|m| m.dev.clone()).collect(),
                faulty: a
                    .members
                    .iter()
                    .filter(|m| m.faulty)
                    .map(|m| m.dev.clone())
                    .collect(),
                sync: a.sync.as_ref().map(|y| SyncOut {
                    action: y.action.clone(),
                    percent: y.percent,
                }),
            })
            .collect();
    out.disks = s
        .disks
        .iter()
        .map(|d| {
            let (smart, smart_error) = match &d.smart_state {
                SmartState::None => ("none", None),
                SmartState::Ok => ("ok", None),
                SmartState::Standby => ("standby", None),
                SmartState::Error { message } => ("error", Some(message.clone())),
                SmartState::Unsupported => ("unsupported", None),
            };
            let m = d.smart.as_ref();
            DiskOut {
                name: d.name.clone(),
                label: d.label.clone(),
                kind: kind(d.kind),
                model: d.model.clone(),
                serial: d.serial.clone(),
                size: d.size,
                pools: d
                    .roles
                    .iter()
                    .filter_map(|r| match r {
                        Role::Pool(p) => Some(p.clone()),
                        Role::Md(_) => None,
                    })
                    .collect(),
                md: d
                    .roles
                    .iter()
                    .filter_map(|r| match r {
                        Role::Md(a) => Some(a.clone()),
                        Role::Pool(_) => None,
                    })
                    .collect(),
                smart,
                smart_error,
                checked: m.map(|m| m.checked),
                passed: m.and_then(|m| m.passed),
                temp: m.and_then(|m| m.temp),
                power_on_hours: m.and_then(|m| m.power_on_hours),
                realloc: m.and_then(|m| m.realloc),
                pending: m.and_then(|m| m.pending),
                uncorrectable: m.and_then(|m| m.uncorrectable),
                crc: m.and_then(|m| m.crc),
                wear: m.and_then(|m| m.wear),
                media_errors: m.and_then(|m| m.media_errors),
                self_test: m.and_then(|m| m.self_test.as_ref()).map(|t| SelfTestOut {
                    kind: t.kind.clone(),
                    passed: t.passed,
                    result: t.result.clone(),
                }),
            }
        })
        .collect();
    let h = &s.host;
    out.host = Some(HostOut {
        uptime: h.uptime_secs,
        load: h.load,
        cpus: h.cpus,
        mem_total: h.mem_total,
        mem_available: h.mem_available,
        cpu_temp: h.cpu_temp,
        arc_size: h.arc.map(|a| a.size),
        arc_hit_rate: h.arc.and_then(|a| a.hit_rate),
        root_fs: h.root_fs.map(|f| FsOut {
            total: f.total,
            used: f.used,
            avail: f.avail,
            percent: f.percent(),
        }),
    });
    out.app = Some(AppOut {
        sessions: s.app.sessions,
        transfers: s.app.transfers,
        shares_active: s.app.shares_active,
        failed_logins_24h: s.app.failed_logins_24h,
        agent_calls_24h: s.app.agent_calls_24h,
        backup: s.app.backup.as_ref().map(|b| BackupOut {
            attempt: b.attempt,
            last_ok: b.last_ok,
            error: b.error.clone(),
            bytes: b.bytes,
        }),
    });
    out
}
