//! The dashboard's activity charts: processor, memory, network and disks over the last
//! 24 hours (per-minute readings as 5-minute means), 7 days (hourly means) or 90 days
//! (daily means: a day's rhythm would turn finer buckets into a comb). The four charts share one time grid, so `assets/dashboard.js` moves one
//! crosshair through all of them and shows every value at that moment. Alerts and the
//! nightly cleanup are marked on the time axis.

use super::charts::{self, Pulse, PulseLine, PulseMark};
use super::views::Ctx;
use crate::health::alerts::LogEntry;
use crate::health::history::{Activity, Sample};
use maud::{Markup, html};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Range {
    Day,
    Week,
    Quarter,
}

impl Range {
    /// `?range=` value; anything else is the last 24 hours.
    pub fn parse(s: &str) -> Range {
        match s {
            "7d" => Range::Week,
            "90d" => Range::Quarter,
            _ => Range::Day,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Range::Day => "24h",
            Range::Week => "7d",
            Range::Quarter => "90d",
        }
    }

    /// Seconds per bucket.
    fn step(self) -> i64 {
        match self {
            Range::Day => 300,
            Range::Week => 3600,
            Range::Quarter => 86_400,
        }
    }

    fn buckets(self) -> usize {
        match self {
            Range::Day => 288,
            Range::Week => 168,
            Range::Quarter => 90,
        }
    }

    fn label_key(self) -> &'static str {
        match self {
            Range::Day => "dashboard.range_24h",
            Range::Week => "dashboard.range_7d",
            Range::Quarter => "dashboard.range_90d",
        }
    }

    fn note_key(self) -> &'static str {
        match self {
            Range::Day => "dashboard.resolution_5m",
            Range::Week => "dashboard.resolution_1h",
            Range::Quarter => "dashboard.resolution_1d",
        }
    }
}

/// What the activity charts are drawn from.
pub struct Input<'a> {
    pub now: i64,
    pub range: Range,
    /// Per-minute readings of the last day, oldest first.
    pub recent: &'a [(i64, Activity)],
    /// Hourly samples, oldest first; `activity` is the mean of the hour before the key.
    pub history: &'a [(i64, Sample)],
    pub mem_total: Option<u64>,
    /// Alert log entries (any order).
    pub log: &'a [LogEntry],
    /// When the daily cleanup and backup run (local hour, minute).
    pub daily_at: (u8, u8),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Unit {
    Percent,
    Bytes,
    Rate,
}

impl Unit {
    fn code(self) -> &'static str {
        match self {
            Unit::Percent => "percent",
            Unit::Bytes => "bytes",
            Unit::Rate => "rate",
        }
    }
}

type Field = fn(&Activity) -> Option<f64>;

struct Chart {
    kind: &'static str,
    title: &'static str,
    unit: Unit,
    /// (name key, field) for the main and the second series.
    a: (&'static str, Field),
    b: (&'static str, Field),
}

const CHARTS: [Chart; 4] = [
    Chart {
        kind: "cpu",
        title: "dashboard.cpu_chart",
        unit: Unit::Percent,
        a: ("dashboard.cpu_busy", |a| a.cpu),
        b: ("dashboard.cpu_wait", |a| a.iowait),
    },
    Chart {
        kind: "memory",
        title: "dashboard.mem_chart",
        unit: Unit::Bytes,
        a: ("dashboard.mem_used", |a| a.mem),
        b: ("dashboard.mem_arc", |a| a.arc),
    },
    Chart {
        kind: "network",
        title: "dashboard.net_chart",
        unit: Unit::Rate,
        a: ("dashboard.net_rx", |a| a.net_rx),
        b: ("dashboard.net_tx", |a| a.net_tx),
    },
    Chart {
        kind: "disks",
        title: "dashboard.io_chart",
        unit: Unit::Rate,
        a: ("dashboard.io_read", |a| a.disk_read),
        b: ("dashboard.io_write", |a| a.disk_write),
    },
];

/// One field's readings as points: the hourly means of the hours before the first
/// per-minute reading (spread over their hour), then the per-minute readings. After a
/// restart the last day is still drawn, coarser.
fn points(inp: &Input, f: Field) -> Vec<(i64, f64)> {
    let first = inp.recent.first().map(|(t, _)| *t).unwrap_or(i64::MAX);
    let mut out = Vec::new();
    for (hour, s) in inp.history.iter().filter(|(h, _)| *h <= first) {
        if let Some(v) = s.activity.as_ref().and_then(f) {
            out.extend((0..12).map(|k| (hour - 3600 + k * 300, v)));
        }
    }
    out.extend(inp.recent.iter().filter_map(|(t, a)| f(a).map(|v| (*t, v))));
    out
}

fn decimal(ctx: &Ctx, v: f64, places: usize) -> String {
    format!("{v:.places$}").replace('.', ctx.t("format.decimal"))
}

fn value_text(ctx: &Ctx, unit: Unit, v: Option<f64>) -> String {
    let Some(v) = v.filter(|v| v.is_finite()) else {
        return "–".into();
    };
    match unit {
        Unit::Percent => {
            let places = if v < 9.95 { 1 } else { 0 };
            format!("{} %", decimal(ctx, v, places))
        }
        Unit::Bytes => ctx.size(v.max(0.0) as u64),
        Unit::Rate => ctx.tf("dashboard.rate", &[("size", &ctx.size(v.max(0.0) as u64))]),
    }
}

/// Values for the script: one per bucket, empty where there is none.
fn data_attr(unit: Unit, values: &[Option<f64>]) -> String {
    values
        .iter()
        .map(|v| match v {
            Some(v) if unit == Unit::Percent => format!("{v:.1}"),
            Some(v) => format!("{v:.0}"),
            None => String::new(),
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn zoned(ctx: &Ctx, t: i64) -> Option<jiff::Zoned> {
    jiff::Timestamp::from_second(t)
        .ok()
        .map(|ts| ts.to_zoned(ctx.tz.clone()))
}

/// Local midnights in `(start, end)`.
fn midnights(ctx: &Ctx, start: i64, end: i64) -> Vec<i64> {
    let mut out = Vec::new();
    let Some(mut z) = zoned(ctx, start).and_then(|z| z.start_of_day().ok()) else {
        return out;
    };
    while let Ok(next) = z.tomorrow().and_then(|d| d.start_of_day()) {
        let t = next.timestamp().as_second();
        if t >= end {
            break;
        }
        out.push(t);
        z = next;
    }
    out
}

/// Time labels: every 6 hours, every other day, or the first of each month.
fn ticks(ctx: &Ctx, range: Range, start: i64, end: i64) -> Vec<(i64, String)> {
    match range {
        Range::Day => {
            let first = start - start.rem_euclid(3600) + 3600;
            (first..end)
                .step_by(3600)
                .filter_map(|t| {
                    let z = zoned(ctx, t)?;
                    (z.hour() % 6 == 0 && z.minute() == 0)
                        .then(|| (t, format!("{:02}:00", z.hour())))
                })
                .collect()
        }
        Range::Week => {
            // Every other day, counted back from the newest, so labels don't crowd.
            let days = midnights(ctx, start, end);
            let last = days.len().saturating_sub(1);
            days.into_iter()
                .enumerate()
                .filter(|(i, _)| (last - i).is_multiple_of(2))
                .map(|(_, t)| (t, ctx.i18n.day(ctx.lang, t, ctx.tz)))
                .collect()
        }
        Range::Quarter => midnights(ctx, start, end)
            .into_iter()
            .filter(|t| zoned(ctx, *t).is_some_and(|z| z.day() == 1))
            .map(|t| (t, ctx.i18n.month(ctx.lang, t, ctx.tz)))
            .collect(),
    }
}

/// The last run of the daily cleanup and backup at or before `now`.
fn last_daily(ctx: &Ctx, now: i64, (h, m): (u8, u8)) -> Option<i64> {
    let today = zoned(ctx, now)?.date();
    [Some(today), today.yesterday().ok()]
        .into_iter()
        .flatten()
        .filter_map(|d| {
            d.at(h as i8, m as i8, 0, 0)
                .to_zoned(ctx.tz.clone())
                .ok()
                .map(|z| z.timestamp().as_second())
        })
        .find(|t| *t <= now)
}

fn marks(ctx: &Ctx, inp: &Input, start: i64, end: i64) -> Vec<PulseMark> {
    let mut log: Vec<&LogEntry> = inp
        .log
        .iter()
        .filter(|e| matches!(e.what.as_str(), "fired" | "escalated" | "event"))
        .filter(|e| (start..end).contains(&e.time))
        .collect();
    log.sort_by_key(|e| std::cmp::Reverse(e.time));
    let mut out: Vec<PulseMark> = log
        .into_iter()
        .take(30)
        .map(|e| PulseMark {
            time: e.time,
            kind: e.severity.as_str(),
            title: format!(
                "{}: {}",
                ctx.datetime(e.time),
                e.msg.render(ctx.i18n, ctx.lang)
            ),
        })
        .collect();
    if inp.range == Range::Day
        && let Some(t) = last_daily(ctx, inp.now, inp.daily_at).filter(|t| *t >= start)
    {
        out.push(PulseMark {
            time: t,
            kind: "job",
            title: ctx.tf(
                "dashboard.job_mark",
                &[(
                    "time",
                    &format!("{:02}:{:02}", inp.daily_at.0, inp.daily_at.1),
                )],
            ),
        });
    }
    out
}

fn summary(ctx: &Ctx, unit: Unit, name: &str, values: &[Option<f64>]) -> Option<String> {
    let vals: Vec<f64> = values.iter().flatten().copied().collect();
    if vals.is_empty() {
        return None;
    }
    let avg = vals.iter().sum::<f64>() / vals.len() as f64;
    let peak = vals.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    Some(ctx.tf(
        "dashboard.pulse_summary",
        &[
            ("series", name),
            ("avg", &value_text(ctx, unit, Some(avg))),
            ("peak", &value_text(ctx, unit, Some(peak))),
        ],
    ))
}

pub fn section(ctx: &Ctx, inp: &Input) -> Markup {
    let range = inp.range;
    let (step, n) = (range.step(), range.buckets());
    let start = inp.now - inp.now.rem_euclid(step) - (n as i64 - 1) * step;
    let end = start + n as i64 * step;
    let ticks = ticks(ctx, range, start, end);
    let marks = marks(ctx, inp, start, end);
    let latest = inp
        .recent
        .last()
        .filter(|(t, _)| inp.now - t < 5 * 60)
        .map(|(_, a)| a);
    let range_label = ctx.t(range.label_key());
    let charts: Vec<Markup> = CHARTS
        .iter()
        .filter_map(|c| {
            let a = charts::bucket_mean(&points(inp, c.a.1), start, step, n);
            let b = charts::bucket_mean(&points(inp, c.b.1), start, step, n);
            if a.iter().chain(&b).all(Option::is_none) {
                return None;
            }
            let top = a.iter().chain(&b).flatten().copied().fold(0.0, f64::max);
            let max = match c.unit {
                Unit::Percent => 100.0,
                Unit::Bytes => inp
                    .mem_total
                    .map(|t| t as f64)
                    .filter(|t| *t >= top)
                    .unwrap_or_else(|| charts::nice_ceil(top)),
                Unit::Rate => charts::nice_ceil(top.max(1000.0)),
            };
            let max_label = value_text(ctx, c.unit, Some(max));
            let (name_a, name_b) = (ctx.t(c.a.0), ctx.t(c.b.0));
            let summary = [
                summary(ctx, c.unit, name_a, &a),
                summary(ctx, c.unit, name_b, &b),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("; ");
            let label = ctx.tf(
                "dashboard.pulse_label",
                &[("chart", ctx.t(c.title)), ("range", range_label), ("summary", &summary)],
            );
            // A series the machine has no readings of (no ZFS: no ARC) stays out entirely.
            let has = |v: &[Option<f64>]| v.iter().any(Option::is_some);
            let lines: Vec<PulseLine> = [
                PulseLine { slot: "a", values: &a },
                PulseLine { slot: "b", values: &b },
            ]
            .into_iter()
            .filter(|l| has(l.values))
            .collect();
            let series: Vec<(&str, &str, Field)> = [("a", name_a, c.a.1, &a), ("b", name_b, c.b.1, &b)]
                .into_iter()
                .filter(|(_, _, _, v)| has(v))
                .map(|(slot, name, f, _)| (slot, name, f))
                .collect();
            Some(html! {
                figure class="pulse" data-kind=(c.kind) data-unit=(c.unit.code()) data-max=(max)
                    data-a=(data_attr(c.unit, &a)) data-b=(data_attr(c.unit, &b)) {
                    figcaption class="pulse__head" {
                        span class="pulse__top" {
                            span class="pulse__title" { (ctx.t(c.title)) }
                            span class="pulse__when" data-js="pulse-when" {}
                        }
                        span class="pulse__legend" {
                            @for (slot, name, field) in series {
                                @let now = latest.and_then(field);
                                @let now_text = value_text(ctx, c.unit, now);
                                span class="pulse__series" data-slot=(slot) {
                                    span class={"pulse__key pulse__key--" (slot)} aria-hidden="true" {}
                                    span class="pulse__num" data-js="pulse-val" data-slot=(slot) data-now=(now_text)
                                        data-state=[now.is_none().then_some("empty")] { (now_text) }
                                    " "
                                    span class="pulse__name" { (name) }
                                }
                            }
                        }
                    }
                    (charts::pulse(&Pulse {
                        start,
                        step,
                        lines: &lines,
                        max,
                        max_label: &max_label,
                        ticks: &ticks,
                        marks: &marks,
                        label: &label,
                    }))
                }
            })
        })
        .collect();
    html! {
        section class="activity" aria-labelledby="dash-activity" data-js="activity"
            data-start=(start) data-step=(step) data-n=(n) data-range=(range.code())
            data-tz=[ctx.tz.iana_name()] data-lang=(ctx.code()) data-decimal=(ctx.t("format.decimal"))
            data-rate=(ctx.t("dashboard.rate")) {
            header class="activity__head" {
                h2 id="dash-activity" class="activity__title" { (ctx.t("dashboard.activity")) }
                nav class="range" aria-label=(ctx.t("dashboard.range_label")) {
                    @for r in [Range::Day, Range::Week, Range::Quarter] {
                        a class="range__item" href=(format!("?range={}", r.code()))
                            aria-current=[(r == range).then_some("true")] { (ctx.t(r.label_key())) }
                    }
                }
                p class="activity__at" {
                    span class="activity__when" data-js="pulse-at" data-now=(ctx.t("dashboard.at_now")) { (ctx.t("dashboard.at_now")) }
                    " "
                    span class="activity__note" { (ctx.t(range.note_key())) }
                }
            }
            @if charts.is_empty() {
                p class="activity__empty" { (ctx.t("dashboard.pulse_empty")) }
            } @else {
                div class="pulse-grid" {
                    @for c in &charts { (c) }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        assert_eq!(Range::parse("7d"), Range::Week);
        assert_eq!(Range::parse("nonsense"), Range::Day);
        for r in [Range::Day, Range::Week, Range::Quarter] {
            assert_eq!(Range::parse(r.code()), r);
        }
    }

    #[test]
    fn hourly_fills_before_minutes() {
        let act = |cpu| Activity {
            cpu: Some(cpu),
            ..Activity::default()
        };
        let hist = vec![
            (
                7200,
                Sample {
                    activity: Some(act(10.0)),
                    ..Sample::default()
                },
            ),
            (
                10800,
                Sample {
                    activity: Some(act(99.0)),
                    ..Sample::default()
                },
            ),
        ];
        let recent = vec![(9000, act(50.0)), (9060, act(70.0))];
        let inp = Input {
            now: 9060,
            range: Range::Day,
            recent: &recent,
            history: &hist,
            mem_total: None,
            log: &[],
            daily_at: (4, 0),
        };
        let p = points(&inp, |a| a.cpu);
        // The hour 1:00–2:00 from the hourly mean (12 points), then the minutes; the
        // later hourly sample overlaps the minutes and is left out.
        assert_eq!(p.len(), 14);
        assert_eq!((p[0], p[11]), ((3600, 10.0), (3600 + 11 * 300, 10.0)));
        assert_eq!(&p[12..], &[(9000, 50.0), (9060, 70.0)]);
    }
}
