//! Admin dashboard (`/.cc/admin/dashboard`) and alerts page (`/.cc/admin/alerts`). Read
//! only: no scrub, offline or replace buttons. Sections for hardware the machine doesn't
//! have are left out. The dashboard reloads itself every 30 s with JS; without JS it is
//! static and its "as of" label tells how old it is.

use super::auth::{audit, csrf_ok, forbidden, now};
use super::charts::{self, FillChart};
use super::files::{Fail, fail};
use super::views::{Ctx, layout, verdict_key};
use super::{ClientInfo, Current, Shared, page_ctx};
use crate::auth::perm::Principal;
use crate::health::alerts::{self, Active, LogEntry, Severity};
use crate::health::disks::{Disk, Kind, Role, SmartState};
use crate::health::history::{self, Sample};
use crate::health::zpool::{Pool, Vdev};
use crate::health::{DatasetInfo, Probe, SmartFile, Snapshot};
use crate::notify::{Kind as ChannelKind, Level, Message};
use axum::extract::{Extension, Form, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use maud::{Markup, html};
use serde::Deserialize;

const DAY: i64 = 86_400;

fn clock(ctx: &Ctx, t: i64) -> String {
    jiff::Timestamp::from_second(t)
        .map(|ts| ts.to_zoned(ctx.tz.clone()).strftime("%H:%M:%S").to_string())
        .unwrap_or_default()
}

fn ago(ctx: &Ctx, now: i64, t: i64) -> String {
    ctx.tf(
        "dashboard.ago",
        &[("age", &alerts::age(ctx.i18n, ctx.lang, now - t))],
    )
}

fn severity_key(s: Severity) -> &'static str {
    match s {
        Severity::Info => "alerts.severity_info",
        Severity::Warning => "alerts.severity_warning",
        Severity::Critical => "alerts.severity_critical",
    }
}

fn active_list(ctx: &Ctx, active: &[Active], now: i64) -> Markup {
    html! {
        ul class="alert-list" {
            @for a in active {
                li class="alert" data-severity=(a.severity.as_str()) {
                    span class="alert__severity" { (ctx.t(severity_key(a.severity))) }
                    " "
                    span class="alert__title" { (a.msg.render(ctx.i18n, ctx.lang)) }
                    " "
                    time class="alert__since" datetime=(a.since) title=(ctx.datetime(a.since)) {
                        (ctx.tf("alerts.since", &[("age", &alerts::age(ctx.i18n, ctx.lang, now - a.since))]))
                    }
                }
            }
        }
    }
}

/// Everything the dashboard shows.
pub struct DashData {
    pub now: i64,
    pub verdict: alerts::Verdict,
    pub snap: Option<std::sync::Arc<Snapshot>>,
    pub active: Vec<Active>,
    /// One-shot events of the last 24 hours, newest first.
    pub recent: Vec<LogEntry>,
    pub history: std::sync::Arc<Vec<(i64, Sample)>>,
    pub thresholds: crate::config::Thresholds,
}

pub async fn page(
    State(st): State<Shared>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
) -> Response {
    let mut ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    if !principal.is_admin() || current.is_none() {
        return fail(&ctx, Fail::NotFound);
    }
    ctx.scripts.push("dashboard.js");
    let now = now();
    let recent = {
        let st = st.clone();
        tokio::task::spawn_blocking(move || st.db.alert_log::<LogEntry>(300).unwrap_or_default())
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|e| e.what == "event" && now - e.time < DAY)
            .collect()
    };
    let data = DashData {
        now,
        verdict: st.health.verdict(now),
        snap: st.health.latest(),
        active: st.health.active(),
        recent,
        history: st.health.history(),
        thresholds: st.cfg.alerts.thresholds,
    };
    dashboard_view(&ctx, &data).into_response()
}

pub fn dashboard_view(ctx: &Ctx, d: &DashData) -> Markup {
    let title = ctx.t("dashboard.title");
    layout(
        ctx,
        title,
        html! {
            div id="dashboard" class="dashboard" data-verdict=(d.verdict.as_str()) data-refresh="30" {
                header class="dashboard__head" {
                    h1 class="dashboard__title" { (title) }
                    p class="verdict" data-state=(d.verdict.as_str()) { (ctx.t(verdict_key(d.verdict.as_str()))) }
                    @if let Some(s) = &d.snap {
                        p class="as-of" data-js="as-of" data-time=(s.taken) {
                            (ctx.tf("dashboard.as_of", &[("time", &clock(ctx, s.taken))]))
                        }
                    } @else {
                        p class="as-of" data-state="waiting" { (ctx.t("dashboard.first_reading")) }
                    }
                    a class="dashboard__alerts-link" href="/.cc/admin/alerts" { (ctx.t("dashboard.alerts_link")) }
                }
                @if !d.active.is_empty() {
                    section class="dashboard__alerts" aria-labelledby="dash-alerts" {
                        h2 id="dash-alerts" { (ctx.tn("dashboard.active_count", d.active.len() as u64)) }
                        (active_list(ctx, &d.active, d.now))
                    }
                }
                @if !d.recent.is_empty() {
                    section class="dashboard__recent" aria-labelledby="dash-recent" {
                        h2 id="dash-recent" { (ctx.t("dashboard.recent")) }
                        ul class="event-list" {
                            @for e in &d.recent {
                                li class="event" data-severity=(e.severity.as_str()) {
                                    time datetime=(e.time) { (clock(ctx, e.time)) }
                                    " " (e.msg.render(ctx.i18n, ctx.lang))
                                }
                            }
                        }
                    }
                }
                @if let Some(s) = &d.snap {
                    (pools_section(ctx, s, d))
                    (datasets_section(ctx, s, d.now))
                    (disks_section(ctx, s, d))
                    (md_section(ctx, s))
                    (host_section(ctx, s, d.now))
                    (app_section(ctx, s))
                }
            }
        },
    )
}

fn section(id: &str, title: &str, body: Markup) -> Markup {
    html! {
        details class={"dash-section dash-section--" (id)} open {
            summary { h2 class="dash-section__title" { (title) } }
            (body)
        }
    }
}

fn probe_error(ctx: &Ctx, what: &str, e: &str) -> Markup {
    html! {
        p class="dash-error" data-state="error" {
            (ctx.tf("dashboard.cant_read", &[("what", what), ("error", e)]))
        }
    }
}

fn state_attr(state: &str) -> String {
    state.to_lowercase()
}

fn vdev_label(ctx: &Ctx, s: &Snapshot, v: &Vdev) -> String {
    match v.disk.as_deref().and_then(|d| s.disk(d)) {
        Some(d) => d.display_name(),
        None if v.kind == "disk" || v.kind == "file" => v.name.clone(),
        None => match v.class.as_str() {
            "normal" => v.name.clone(),
            class => format!("{} ({})", v.name, ctx.t(class_key(class))),
        },
    }
}

fn class_key(class: &str) -> &'static str {
    match class {
        "special" => "dashboard.vdev_special",
        "dedup" => "dashboard.vdev_dedup",
        "log" => "dashboard.vdev_log",
        _ => "dashboard.vdev_other",
    }
}

fn vdev_tree(ctx: &Ctx, s: &Snapshot, vdevs: &[Vdev]) -> Markup {
    html! {
        ul class="vdev-tree" {
            @for v in vdevs {
                li class="vdev" data-state=(state_attr(&v.state)) data-kind=(v.kind) {
                    span class="vdev__name" { (vdev_label(ctx, s, v)) }
                    " "
                    span class="vdev__state" { (v.state) }
                    @if v.read + v.write + v.cksum > 0 || v.is_leaf() {
                        " "
                        span class="vdev__errors" data-state=(if v.read + v.write + v.cksum > 0 { "errors" } else { "ok" })
                            title=(ctx.t("dashboard.vdev_errors_title")) {
                            (ctx.tf("dashboard.vdev_errors", &[
                                ("read", &v.read.to_string()),
                                ("write", &v.write.to_string()),
                                ("cksum", &v.cksum.to_string()),
                            ]))
                        }
                    }
                    @if !v.children.is_empty() {
                        (vdev_tree(ctx, s, &v.children))
                    }
                }
            }
        }
    }
}

fn scan_line(ctx: &Ctx, p: &Pool, now: i64) -> Markup {
    let Some(sc) = &p.scan else {
        return html! { p class="pool__scan" { (ctx.t("dashboard.never_scrubbed")) } };
    };
    let pct = sc.percent();
    html! {
        @if sc.running() {
            p class="pool__scan" data-state="running" {
                (ctx.tf(if sc.is_resilver() { "dashboard.resilver_running" } else { "dashboard.scrub_running" },
                    &[("percent", &format!("{pct:.0}"))]))
                @if sc.paused { " " (ctx.t("dashboard.paused")) }
                " "
                progress class="pool__progress" max="100" value=(format!("{pct:.0}")) { (format!("{pct:.0} %")) }
            }
        } @else {
            @let end = sc.end.unwrap_or(sc.start);
            p class="pool__scan" data-state=(if sc.errors > 0 { "errors" } else if sc.state == "CANCELED" { "canceled" } else { "ok" }) {
                (ctx.tf(
                    match (sc.is_resilver(), sc.state.as_str()) {
                        (true, _) => "dashboard.last_resilver",
                        (false, "CANCELED") => "dashboard.last_scrub_canceled",
                        _ => "dashboard.last_scrub",
                    },
                    &[
                        ("date", &ctx.datetime(end)),
                        ("ago", &ago(ctx, now, end)),
                        ("errors", &sc.errors.to_string()),
                        ("duration", &alerts::age(ctx.i18n, ctx.lang, end - sc.start)),
                    ],
                ))
            }
        }
    }
}

fn pool_chart(ctx: &Ctx, p: &Pool, d: &DashData) -> Markup {
    let start = d.now - history::KEEP_SECS;
    let raw = history::series(&d.history, |s| s.pools.get(&p.name).map(|(a, _)| *a as f64));
    let percent: Vec<(i64, f64)> = history::series(&d.history, |s| {
        s.pools
            .get(&p.name)
            .filter(|(_, size)| *size > 0)
            .map(|(a, size)| *a as f64 * 100.0 / *size as f64)
    });
    let full_at = history::full_at(&raw, p.size as f64, d.now);
    let projection = match full_at {
        Some(t) => {
            let days = (t - d.now) / DAY;
            if days < 60 {
                ctx.tn("dashboard.full_in_days", days.max(1) as u64)
            } else {
                ctx.tn("dashboard.full_in_months", (days / 30) as u64)
            }
        }
        None if percent.len() < 24 * 7 => ctx.t("dashboard.no_history").to_string(),
        None => ctx.t("dashboard.not_filling").to_string(),
    };
    let daily = charts::bucket_last(&percent, DAY);
    let label = ctx.tf(
        "dashboard.fill_chart_label",
        &[("pool", &p.name), ("projection", &projection)],
    );
    html! {
        figure class="pool__chart" {
            @if daily.len() >= 2 {
            (charts::fill_chart(&FillChart {
                points: &daily,
                start,
                now: d.now,
                ahead: 30 * DAY,
                full_at,
                thresholds: &[(d.thresholds.pool_warn as f64, "warning"), (d.thresholds.pool_crit as f64, "critical")],
                label: &label,
                start_label: ctx.t("dashboard.chart_start"),
                now_label: ctx.t("dashboard.chart_now"),
            }))
            }
            figcaption class="pool__projection" { (projection) }
        }
    }
}

fn pools_section(ctx: &Ctx, s: &Snapshot, d: &DashData) -> Markup {
    let body = match &s.pools {
        Probe::Absent => return html! {},
        Probe::Failed(e) => probe_error(ctx, "zpool", e),
        Probe::Ok(pools) if pools.is_empty() => html! { p { (ctx.t("dashboard.no_pools")) } },
        Probe::Ok(pools) => html! {
            @for p in pools {
                article class="pool" data-state=(state_attr(&p.state)) {
                    h3 class="pool__name" { (p.name) " " span class="pool__state" { (p.state) } }
                    meter class="pool__meter" min="0" max="100" low=(d.thresholds.pool_warn) high=(d.thresholds.pool_crit)
                        optimum="0" value=(p.capacity) { (p.capacity) " %" }
                    p class="pool__space" {
                        (ctx.tf("dashboard.pool_space", &[
                            ("percent", &p.capacity.to_string()),
                            ("free", &ctx.size(p.free)),
                            ("size", &ctx.size(p.size)),
                        ]))
                        @if let Some(f) = p.frag {
                            " · " (ctx.tf("dashboard.frag", &[("percent", &f.to_string())]))
                        }
                    }
                    @if p.data_errors > 0 {
                        p class="pool__data-errors" data-state="critical" {
                            (ctx.tn("dashboard.data_errors", p.data_errors))
                        }
                    }
                    (scan_line(ctx, p, d.now))
                    @if let Some(ex) = p.expand.as_ref().filter(|e| e.running()) {
                        p class="pool__expand" data-state="running" {
                            (ctx.tf("dashboard.expanding", &[("vdev", &ex.vdev), ("percent", &format!("{:.0}", ex.percent()))]))
                        }
                    }
                    (vdev_tree(ctx, s, &p.vdevs))
                    @if !p.spares.is_empty() {
                        h4 class="pool__group" { (ctx.t("dashboard.spares")) }
                        (vdev_tree(ctx, s, &p.spares))
                    }
                    @if !p.cache.is_empty() {
                        h4 class="pool__group" { (ctx.t("dashboard.cache")) }
                        (vdev_tree(ctx, s, &p.cache))
                    }
                    (pool_chart(ctx, p, d))
                }
            }
        },
    };
    section("pools", ctx.t("dashboard.pools"), body)
}

fn datasets_section(ctx: &Ctx, s: &Snapshot, now: i64) -> Markup {
    let body = match &s.datasets {
        Probe::Absent => return html! {},
        Probe::Failed(e) => probe_error(ctx, "zfs", e),
        Probe::Ok(list) => html! {
            div class="table-scroll" {
                table class="dataset-table" {
                    thead { tr {
                        th scope="col" { (ctx.t("dashboard.ds_name")) }
                        th scope="col" { (ctx.t("dashboard.ds_volumes")) }
                        th scope="col" class="num" { (ctx.t("dashboard.ds_used")) }
                        th scope="col" class="num" { (ctx.t("dashboard.ds_avail")) }
                        th scope="col" class="num" { (ctx.t("dashboard.ds_quota")) }
                        th scope="col" class="num" { (ctx.t("dashboard.ds_snapshots")) }
                        th scope="col" { (ctx.t("dashboard.ds_newest")) }
                        th scope="col" class="num" { (ctx.t("dashboard.ds_trash")) }
                    } }
                    tbody {
                        @for i in list { (dataset_row(ctx, i, now)) }
                    }
                }
            }
        },
    };
    section("datasets", ctx.t("dashboard.datasets"), body)
}

fn dataset_row(ctx: &Ctx, i: &DatasetInfo, now: i64) -> Markup {
    let ds = &i.ds;
    html! {
        tr class="dataset" {
            th scope="row" class="dataset__name" { (ds.name) }
            td { (i.volumes.join(", ")) }
            td class="num" { (ctx.size(ds.used)) }
            td class="num" { (ctx.size(ds.avail)) }
            td class="num" {
                @if let Some(pct) = ds.quota_percent() {
                    (ctx.tf("dashboard.ds_quota_value", &[("percent", &pct.to_string()), ("quota", &ctx.size(ds.quota))]))
                } @else { "–" }
            }
            td class="num" {
                (ctx.number(ds.snapshots))
                @if ds.snap_used > 0 { " (" (ctx.size(ds.snap_used)) ")" }
            }
            td {
                @if let Some(t) = ds.newest_snapshot {
                    time datetime=(t) title=(ctx.datetime(t)) { (ago(ctx, now, t)) }
                } @else { "–" }
            }
            td class="num" { @if i.trash > 0 { (ctx.size(i.trash)) } @else { "–" } }
        }
    }
}

fn kind_key(k: Kind) -> &'static str {
    match k {
        Kind::Hdd => "dashboard.kind_hdd",
        Kind::Ssd => "dashboard.kind_ssd",
        Kind::Nvme => "dashboard.kind_nvme",
    }
}

fn roles(ctx: &Ctx, d: &Disk) -> String {
    if d.roles.is_empty() {
        return ctx.t("dashboard.role_other").to_string();
    }
    d.roles
        .iter()
        .map(|r| match r {
            Role::Pool(p) => ctx.tf("dashboard.role_pool", &[("name", p)]),
            Role::Md(m) => ctx.tf("dashboard.role_md", &[("name", m)]),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn smart_cell(ctx: &Ctx, d: &Disk) -> Markup {
    let (state, text) = match (&d.smart_state, d.smart.as_ref().and_then(|s| s.passed)) {
        (SmartState::None, _) => ("none", ctx.t("dashboard.smart_none").to_string()),
        (SmartState::Unsupported, _) => ("none", ctx.t("dashboard.smart_unsupported").to_string()),
        (SmartState::Error { message }, _) => (
            "error",
            ctx.tf("dashboard.smart_error", &[("error", message)]),
        ),
        (_, Some(false)) => ("critical", ctx.t("dashboard.smart_failing").to_string()),
        (SmartState::Standby, _) => ("standby", ctx.t("dashboard.smart_standby").to_string()),
        (_, _) => ("ok", ctx.t("dashboard.smart_ok").to_string()),
    };
    html! { span class="disk__smart" data-state=(state) { (text) } }
}

fn opt(ctx: &Ctx, v: Option<u64>) -> String {
    v.map(|n| ctx.number(n)).unwrap_or_else(|| "–".into())
}

fn disks_section(ctx: &Ctx, s: &Snapshot, d: &DashData) -> Markup {
    if s.disks.is_empty() {
        return html! {};
    }
    let start = d.now - history::KEEP_SECS;
    let stale = match &s.smart {
        SmartFile::Ok { collected } => {
            let age = d.now - collected;
            (age > i64::from(d.thresholds.stale_minutes) * 60).then(|| {
                ctx.tf(
                    "dashboard.smart_stale",
                    &[("age", &alerts::age(ctx.i18n, ctx.lang, age))],
                )
            })
        }
        SmartFile::Missing => Some(ctx.t("dashboard.smart_missing").to_string()),
        SmartFile::Failed { message } => {
            Some(ctx.tf("dashboard.smart_file_error", &[("error", message)]))
        }
    };
    let body = html! {
        @if let Some(msg) = stale {
            p class="dash-error" data-state="stale" { (msg) }
        } @else if let SmartFile::Ok { collected } = &s.smart {
            p class="dash-note" { (ctx.tf("dashboard.smart_as_of", &[("ago", &ago(ctx, d.now, *collected))])) }
        }
        div class="table-scroll" {
            table class="disk-table" {
                thead { tr {
                    th scope="col" { (ctx.t("dashboard.disk_name")) }
                    th scope="col" { (ctx.t("dashboard.disk_model")) }
                    th scope="col" { (ctx.t("dashboard.disk_role")) }
                    th scope="col" { (ctx.t("dashboard.disk_smart")) }
                    th scope="col" class="num" { (ctx.t("dashboard.disk_temp")) }
                    th scope="col" class="num" { (ctx.t("dashboard.disk_hours")) }
                    th scope="col" class="num" { (ctx.t("dashboard.disk_sectors")) }
                    th scope="col" class="num" { (ctx.t("dashboard.disk_crc")) }
                    th scope="col" class="num" { (ctx.t("dashboard.disk_wear")) }
                    th scope="col" { (ctx.t("dashboard.disk_selftest")) }
                } }
                tbody {
                    @for disk in &s.disks {
                        @let sm = disk.smart.as_ref();
                        @let id = disk.id();
                        @let temps = charts::bucket_max(&history::series(&d.history, |h| h.temps.get(&id).map(|t| *t as f64)), DAY / 2);
                        @let realloc = history::series(&d.history, |h| h.realloc.get(&id).map(|r| *r as f64));
                        tr class="disk" data-kind=(match disk.kind { Kind::Hdd => "hdd", Kind::Ssd => "ssd", Kind::Nvme => "nvme" }) {
                            th scope="row" class="disk__name" {
                                (disk.label.clone().unwrap_or_else(|| disk.name.clone()))
                                @if disk.label.is_some() { " " span class="disk__dev" { (disk.name) } }
                            }
                            td class="disk__model" {
                                (disk.model)
                                @if let Some(sn) = disk.short_serial() { " " span class="disk__serial" title=(disk.serial.clone().unwrap_or_default()) { (sn) } }
                                " " span class="disk__kind" { (ctx.t(kind_key(disk.kind))) " · " (ctx.size(disk.size)) }
                            }
                            td class="disk__role" { (roles(ctx, disk)) }
                            td { (smart_cell(ctx, disk)) }
                            td class="num disk__temp" {
                                @if let Some(t) = sm.and_then(|s| s.temp) {
                                    (t) " °C "
                                    (charts::sparkline(&temps, start, d.now, &ctx.tf("dashboard.temp_chart_label", &[("disk", &disk.display_name())]), "sparkline--temp"))
                                } @else { "–" }
                            }
                            td class="num" { (opt(ctx, sm.and_then(|s| s.power_on_hours))) }
                            td class="num disk__sectors" {
                                (ctx.tf("dashboard.sectors_value", &[
                                    ("realloc", &opt(ctx, sm.and_then(|s| s.realloc))),
                                    ("pending", &opt(ctx, sm.and_then(|s| s.pending))),
                                    ("uncorrectable", &opt(ctx, sm.and_then(|s| s.uncorrectable))),
                                ]))
                                @if realloc.iter().any(|(_, v)| *v > 0.0) {
                                    " " (charts::sparkline(&realloc, start, d.now, &ctx.tf("dashboard.realloc_chart_label", &[("disk", &disk.display_name())]), "sparkline--realloc"))
                                }
                            }
                            td class="num" { (opt(ctx, sm.and_then(|s| s.crc))) }
                            td class="num" {
                                @if let Some(w) = sm.and_then(|s| s.wear) { (w) " %" } @else { "–" }
                                @if let Some(m) = sm.and_then(|s| s.media_errors).filter(|m| *m > 0) {
                                    " " span class="disk__media-errors" data-state="errors" { (ctx.tn("dashboard.media_errors", m)) }
                                }
                            }
                            td class="disk__selftest" {
                                @if let Some(t) = sm.and_then(|s| s.self_test.as_ref()) {
                                    span data-state=(match t.passed { Some(true) => "ok", Some(false) => "critical", None => "none" }) {
                                        (t.kind) ": " (t.result)
                                    }
                                    @if let Some(at) = sm.and_then(|s| s.self_test_time()) {
                                        " " time datetime=(at) { (ctx.tf("dashboard.approx", &[("ago", &ago(ctx, d.now, at))])) }
                                    }
                                } @else { "–" }
                                @if sm.is_some_and(|s| s.self_test_running) {
                                    " " span data-state="running" { (ctx.t("dashboard.selftest_running")) }
                                }
                            }
                        }
                    }
                }
            }
        }
    };
    section("disks", ctx.t("dashboard.disks"), body)
}

fn md_section(ctx: &Ctx, s: &Snapshot) -> Markup {
    let body = match &s.md {
        Probe::Absent => return html! {},
        Probe::Failed(e) => probe_error(ctx, "/proc/mdstat", e),
        Probe::Ok(arrays) => html! {
            @for a in arrays {
                @let state = if a.inactive() { "inactive" } else if a.degraded() { "degraded" } else { "ok" };
                article class="md-array" data-state=(state) {
                    h3 class="md-array__name" { (a.name) " " span class="md-array__level" { (a.level) } }
                    p class="md-array__state" {
                        (ctx.t(match state { "inactive" => "dashboard.md_inactive", "degraded" => "dashboard.md_degraded", _ => "dashboard.md_ok" }))
                        @if !a.status.is_empty() { " [" (a.status) "]" }
                    }
                    @if let Some(sync) = &a.sync {
                        p class="md-array__sync" data-state="running" {
                            (ctx.tf("dashboard.md_sync", &[("action", &sync.action), ("percent", &format!("{:.1}", sync.percent))]))
                            " " progress max="100" value=(format!("{:.1}", sync.percent)) {}
                        }
                    }
                    ul class="md-array__members" {
                        @for m in &a.members {
                            li data-state=(if m.faulty { "faulty" } else if m.spare { "spare" } else { "ok" }) {
                                (m.dev)
                                @if let Some(d) = m.disk.as_deref().and_then(|d| s.disk(d)) { " – " (d.display_name()) }
                                @if m.faulty { " " (ctx.t("dashboard.md_faulty")) }
                                @if m.spare { " " (ctx.t("dashboard.md_spare")) }
                            }
                        }
                    }
                }
            }
        },
    };
    section("md", ctx.t("dashboard.md"), body)
}

fn host_section(ctx: &Ctx, s: &Snapshot, _now: i64) -> Markup {
    let h = &s.host;
    let body = html! {
        dl class="stats" {
            @if let Some(up) = h.uptime_secs {
                dt { (ctx.t("dashboard.uptime")) } dd { (alerts::age(ctx.i18n, ctx.lang, up as i64)) }
            }
            @if let Some(l) = h.load {
                dt { (ctx.t("dashboard.load")) }
                dd {
                    (format!("{:.2} {:.2} {:.2}", l[0], l[1], l[2]))
                    @if let Some(c) = h.cpus { " " (ctx.tn("dashboard.cpus", c as u64)) }
                }
            }
            @if let (Some(total), Some(avail)) = (h.mem_total, h.mem_available) {
                dt { (ctx.t("dashboard.memory")) }
                dd {
                    (ctx.tf("dashboard.used_of", &[("used", &ctx.size(total.saturating_sub(avail))), ("total", &ctx.size(total))]))
                    " " meter min="0" max=(total) value=(total.saturating_sub(avail)) {}
                }
            }
            @if let Some(arc) = h.arc {
                dt { (ctx.t("dashboard.arc")) }
                dd {
                    (ctx.tf("dashboard.used_of", &[("used", &ctx.size(arc.size)), ("total", &ctx.size(arc.max))]))
                    @if let Some(r) = arc.hit_rate { " · " (ctx.tf("dashboard.hit_rate", &[("percent", &format!("{r:.1}"))])) }
                }
            }
            @if let Some(t) = h.cpu_temp {
                dt { (ctx.t("dashboard.cpu_temp")) } dd { (format!("{t:.0}")) " °C" }
            }
            @if let Some(fs) = h.root_fs {
                dt { (ctx.t("dashboard.root_fs")) }
                dd {
                    (ctx.tf("dashboard.used_of", &[("used", &ctx.size(fs.used)), ("total", &ctx.size(fs.total))]))
                    " (" (fs.percent()) " %) "
                    meter min="0" max="100" value=(fs.percent()) {}
                }
            }
        }
    };
    section("host", ctx.t("dashboard.host"), body)
}

fn app_section(ctx: &Ctx, s: &Snapshot) -> Markup {
    let a = &s.app;
    let body = html! {
        dl class="stats" {
            dt { (ctx.t("dashboard.sessions")) } dd { (a.sessions) }
            dt { (ctx.t("dashboard.transfers")) } dd { (a.transfers) }
            dt { (ctx.t("dashboard.shares")) } dd { a href="/.cc/shares?all=1" { (a.shares_active) } }
            dt { (ctx.t("dashboard.failed_logins")) } dd { (a.failed_logins_24h) }
            @if !a.agents.is_empty() {
                dt { (ctx.t("dashboard.agent_calls")) } dd { (a.agent_calls_24h) }
            }
        }
        @if !a.agents.is_empty() {
            ul class="dash-agents" {
                @for ag in &a.agents {
                    li data-state=(if ag.disabled { "off" } else { "ok" }) {
                        @match ag.last_call {
                            Some(t) => (ctx.tf("dashboard.agent_last", &[("agent", &ag.name), ("role", &ag.role), ("time", &ctx.datetime(t))])),
                            None => (ctx.tf("dashboard.agent_never", &[("agent", &ag.name), ("role", &ag.role)])),
                        }
                    }
                }
            }
        }
        @if !a.missing_roles.is_empty() {
            ul class="dash-warnings" {
                @for (user, role) in &a.missing_roles {
                    li data-state="warning" { (ctx.tf("dashboard.role_missing", &[("user", user), ("role", role)])) }
                }
            }
        }
    };
    section("app", ctx.t("dashboard.app"), body)
}

// ---------------------------------------------------------------------------------------
// Alerts page

pub struct ChannelRow {
    pub kind: &'static str,
    pub destination: String,
    pub problem: Option<String>,
    pub status: crate::notify::Status,
}

pub struct AlertsData {
    pub now: i64,
    pub active: Vec<Active>,
    pub log: Vec<LogEntry>,
    pub channels: Vec<ChannelRow>,
    pub csrf: String,
    /// Result of a test message: (ok, text).
    pub flash: Option<(bool, String)>,
}

fn what_key(what: &str) -> &'static str {
    match what {
        "fired" => "alerts.what_fired",
        "escalated" => "alerts.what_escalated",
        "repeated" => "alerts.what_repeated",
        "resolved" => "alerts.what_resolved",
        _ => "alerts.what_event",
    }
}

pub fn alerts_view(ctx: &Ctx, d: &AlertsData) -> Markup {
    let title = ctx.t("alerts.title");
    layout(
        ctx,
        title,
        html! {
            h1 { (title) }
            @if let Some((ok, text)) = &d.flash {
                p class="flash" data-state=(if *ok { "success" } else { "error" }) role="status" { (text) }
            }
            section class="alerts-active" aria-labelledby="alerts-active" {
                h2 id="alerts-active" { (ctx.t("alerts.active")) }
                @if d.active.is_empty() {
                    p class="alerts-active__none" { (ctx.t("alerts.none_active")) }
                } @else {
                    (active_list(ctx, &d.active, d.now))
                }
            }
            section class="alert-channels" aria-labelledby="alert-channels" {
                h2 id="alert-channels" { (ctx.t("alerts.channels")) }
                @if d.channels.is_empty() {
                    p class="alert-channels__none" { (ctx.t("alerts.no_channels")) }
                }
                @for c in &d.channels {
                    article class="channel" data-state=(if c.problem.is_some() { "off" } else if c.status.last_error_at > c.status.last_ok { "error" } else { "ok" }) {
                        h3 class="channel__name" { (ctx.t(if c.kind == "telegram" { "alerts.telegram" } else { "alerts.ntfy" })) }
                        p class="channel__dest" { (c.destination) }
                        @if let Some(p) = &c.problem {
                            p class="channel__problem" { (p) }
                        }
                        dl class="stats" {
                            dt { (ctx.t("alerts.last_ok")) }
                            dd { @if let Some(t) = c.status.last_ok { (ctx.datetime(t)) } @else { (ctx.t("alerts.never")) } }
                            @if let (Some(t), Some(e)) = (c.status.last_error_at, &c.status.last_error) {
                                dt { (ctx.t("alerts.last_error")) }
                                dd { (ctx.datetime(t)) ": " (e) }
                            }
                        }
                        @if c.problem.is_none() {
                            form method="post" action="/.cc/admin/alerts" class="channel__test" {
                                input type="hidden" name="csrf" value=(d.csrf);
                                input type="hidden" name="op" value="test";
                                input type="hidden" name="channel" value=(c.kind);
                                button type="submit" { (ctx.t("alerts.send_test")) }
                            }
                        }
                    }
                }
            }
            section class="alert-history" aria-labelledby="alert-history" {
                h2 id="alert-history" { (ctx.t("alerts.history")) }
                @if d.log.is_empty() {
                    p { (ctx.t("alerts.no_history")) }
                } @else {
                    div class="table-scroll" {
                        table class="alert-history__table" {
                            thead { tr {
                                th scope="col" { (ctx.t("alerts.col_time")) }
                                th scope="col" { (ctx.t("alerts.col_what")) }
                                th scope="col" { (ctx.t("alerts.col_message")) }
                            } }
                            tbody {
                                @for e in &d.log {
                                    tr data-severity=(e.severity.as_str()) data-what=(e.what) {
                                        td { time datetime=(e.time) { (ctx.datetime(e.time)) } }
                                        td { (ctx.t(what_key(&e.what))) " · " (ctx.t(severity_key(e.severity))) }
                                        td { (e.msg.render(ctx.i18n, ctx.lang)) }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        },
    )
}

async fn alerts_data(st: &Shared, csrf: String, flash: Option<(bool, String)>) -> AlertsData {
    let log = {
        let st = st.clone();
        tokio::task::spawn_blocking(move || st.db.alert_log::<LogEntry>(200).unwrap_or_default())
            .await
            .unwrap_or_default()
    };
    let channels = {
        let st = st.clone();
        tokio::task::spawn_blocking(move || {
            st.notifier
                .channels()
                .iter()
                .map(|c| ChannelRow {
                    kind: c.kind.name(),
                    destination: c.destination(),
                    problem: c.problem.clone(),
                    status: st.notifier.status(c.kind),
                })
                .collect()
        })
        .await
        .unwrap_or_default()
    };
    AlertsData {
        now: now(),
        active: st.health.active(),
        log,
        channels,
        csrf,
        flash,
    }
}

pub async fn alerts_page(
    State(st): State<Shared>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
) -> Response {
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    let Some(cu) = current.as_deref().filter(|_| principal.is_admin()) else {
        return fail(&ctx, Fail::NotFound);
    };
    alerts_view(&ctx, &alerts_data(&st, cu.csrf.clone(), None).await).into_response()
}

#[derive(Deserialize, Default)]
pub struct AlertsForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    op: String,
    #[serde(default)]
    channel: String,
}

pub async fn alerts_submit(
    State(st): State<Shared>,
    Extension(client): Extension<ClientInfo>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
    Form(form): Form<AlertsForm>,
) -> Response {
    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    let Some(cu) = current.as_deref().filter(|_| principal.is_admin()) else {
        return fail(&ctx, Fail::NotFound);
    };
    if !csrf_ok(&headers, &st, cu, &form.csrf) {
        return forbidden(&ctx);
    }
    let (Some(kind), "test") = (ChannelKind::parse(&form.channel), form.op.as_str()) else {
        return fail(&ctx, Fail::BadRequest);
    };
    let lang = super::monitor::alert_lang(&st);
    let msg = Message {
        level: Level::Info,
        title: st.i18n.t(lang, "alert.test").to_string(),
        body: st.i18n.tf(
            lang,
            "alert.test_body",
            &[
                ("user", &cu.name),
                (
                    "time",
                    &st.i18n.datetime(lang, now(), &st.cfg.server.timezone),
                ),
            ],
        ),
    };
    let result = st.notifier.send_now(kind, &msg).await;
    audit(
        &st,
        format!("user:{}", cu.key),
        client.ip,
        "alert_test",
        kind.name(),
        match &result {
            Ok(()) => "ok",
            Err(_) => "failed",
        },
    );
    let channel_name = ctx.t(if kind == ChannelKind::Telegram {
        "alerts.telegram"
    } else {
        "alerts.ntfy"
    });
    let flash = match &result {
        Ok(()) => (
            true,
            ctx.tf("alerts.test_sent", &[("channel", channel_name)]),
        ),
        Err(e) => (
            false,
            ctx.tf(
                "alerts.test_failed",
                &[("channel", channel_name), ("error", e)],
            ),
        ),
    };
    let status = if result.is_ok() {
        StatusCode::OK
    } else {
        StatusCode::BAD_GATEWAY
    };
    (
        status,
        alerts_view(&ctx, &alerts_data(&st, cu.csrf.clone(), Some(flash)).await),
    )
        .into_response()
}
