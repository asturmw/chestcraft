//! Server-side SVG charts for the dashboard: sparklines, a fill chart with thresholds and a
//! projection, and the activity charts (`pulse`). Plain SVG with class names; colors and
//! strokes come from the CSS.

use maud::{Markup, html};

/// One point per `step` seconds: the largest value in each bucket.
pub fn bucket_max(points: &[(i64, f64)], step: i64) -> Vec<(i64, f64)> {
    let mut out: Vec<(i64, f64)> = Vec::new();
    for (t, v) in points {
        let b = t - t.rem_euclid(step);
        match out.last_mut() {
            Some((lt, lv)) if *lt == b => *lv = lv.max(*v),
            _ => out.push((b, *v)),
        }
    }
    out
}

/// One point per `step` seconds: the last value in each bucket.
pub fn bucket_last(points: &[(i64, f64)], step: i64) -> Vec<(i64, f64)> {
    let mut out: Vec<(i64, f64)> = Vec::new();
    for (t, v) in points {
        let b = t - t.rem_euclid(step);
        match out.last_mut() {
            Some((lt, lv)) if *lt == b => *lv = *v,
            _ => out.push((b, *v)),
        }
    }
    out
}

fn fmt(v: f64) -> String {
    format!("{:.1}", v)
}

fn polyline(points: &[(f64, f64)]) -> String {
    points
        .iter()
        .map(|(x, y)| format!("{},{}", fmt(*x), fmt(*y)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A small trend line over `[t0, t1]`, scaled to its own range.
pub fn sparkline(points: &[(i64, f64)], t0: i64, t1: i64, label: &str, class: &str) -> Markup {
    const W: f64 = 120.0;
    const H: f64 = 24.0;
    if points.len() < 2 || t1 <= t0 {
        return html! {};
    }
    let lo = points.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
    let hi = points.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);
    let span = (hi - lo).max(1.0);
    let xy: Vec<(f64, f64)> = points
        .iter()
        .map(|(t, v)| {
            (
                (t - t0) as f64 / (t1 - t0) as f64 * W,
                H - 2.0 - (v - lo) / span * (H - 4.0),
            )
        })
        .collect();
    html! {
        svg class={"sparkline " (class)} viewBox="0 0 120 24" preserveAspectRatio="none"
            role="img" aria-label=(label) {
            polyline class="sparkline__line" fill="none" stroke="currentColor"
                vector-effect="non-scaling-stroke" points=(polyline(&xy)) {}
        }
    }
}

/// The mean of the values in each of `n` buckets of `step` seconds from `start`.
pub fn bucket_mean(points: &[(i64, f64)], start: i64, step: i64, n: usize) -> Vec<Option<f64>> {
    let mut sum = vec![(0.0, 0u32); n];
    for (t, v) in points {
        if *t < start || step <= 0 {
            continue;
        }
        let i = ((t - start) / step) as usize;
        if let Some(slot) = sum.get_mut(i) {
            slot.0 += v;
            slot.1 += 1;
        }
    }
    sum.into_iter()
        .map(|(s, c)| (c > 0).then(|| s / c as f64))
        .collect()
}

/// A round top for a scale that holds `v`: 1, 2, 2.5 or 5 times a power of ten.
pub fn nice_ceil(v: f64) -> f64 {
    if v.is_nan() || v <= 0.0 || !v.is_finite() {
        return 1.0;
    }
    let p = 10f64.powf(v.log10().floor());
    [1.0, 2.0, 2.5, 5.0, 10.0]
        .into_iter()
        .map(|m| m * p)
        .find(|c| *c >= v * 0.999)
        .unwrap_or(10.0 * p)
}

/// One series of a pulse chart: `a` is the main one (line over a wash), `b` the second
/// (a line). One value per bucket, None where there was no reading.
pub struct PulseLine<'a> {
    pub slot: &'static str,
    pub values: &'a [Option<f64>],
}

/// Something that happened at a moment: an alert, the nightly cleanup.
pub struct PulseMark {
    pub time: i64,
    /// `critical`, `warning`, `info` or `job`.
    pub kind: &'static str,
    pub title: String,
}

pub struct Pulse<'a> {
    /// Start of bucket 0; buckets are `step` seconds.
    pub start: i64,
    pub step: i64,
    pub lines: &'a [PulseLine<'a>],
    /// Top of the scale.
    pub max: f64,
    pub max_label: &'a str,
    /// Time labels along the bottom.
    pub ticks: &'a [(i64, String)],
    pub marks: &'a [PulseMark],
    pub label: &'a str,
}

/// Path data for the runs of `values` (x in 0–1000, y in 0–100 with 0 at the top). With
/// `close`, each run becomes an area down to the baseline.
fn pulse_path(values: &[Option<f64>], max: f64, close: bool) -> String {
    let n = values.len().max(1) as f64;
    let mut d = String::new();
    let mut run: Vec<(f64, f64)> = Vec::new();
    let flush = |run: &mut Vec<(f64, f64)>, d: &mut String| {
        if run.is_empty() {
            return;
        }
        let (x0, x1) = (run[0].0, run[run.len() - 1].0);
        if close {
            d.push_str(&format!("M{},100", fmt(x0)));
            for (x, y) in run.iter() {
                d.push_str(&format!("L{},{}", fmt(*x), fmt(*y)));
            }
            d.push_str(&format!("L{},100Z", fmt(x1)));
        } else {
            for (i, (x, y)) in run.iter().enumerate() {
                d.push_str(&format!(
                    "{}{},{}",
                    if i == 0 { 'M' } else { 'L' },
                    fmt(*x),
                    fmt(*y)
                ));
            }
            if run.len() == 1 {
                // A lone reading: a dot (round caps on a zero-length line).
                d.push_str(&format!("L{},{}", fmt(x0), fmt(run[0].1)));
            }
        }
        run.clear();
    };
    for (i, v) in values.iter().enumerate() {
        match v {
            Some(v) if v.is_finite() => {
                let x = (i as f64 + 0.5) / n * 1000.0;
                let y = 100.0 - (v / max).clamp(0.0, 1.0) * 100.0;
                run.push((x, y));
            }
            _ => flush(&mut run, &mut d),
        }
    }
    flush(&mut run, &mut d);
    d
}

/// An activity chart: the data stretches with the width (strokes keep their width), the
/// grid, marks and labels sit at percentages so their text never stretches. The crosshair
/// and its dots are moved by `assets/dashboard.js`.
pub fn pulse(c: &Pulse) -> Markup {
    let n = c.lines.iter().map(|l| l.values.len()).max().unwrap_or(0);
    let span = (n as i64 * c.step).max(1);
    let pct = |t: i64| {
        format!(
            "{:.2}%",
            ((t - c.start) as f64 / span as f64 * 100.0).clamp(0.0, 100.0)
        )
    };
    let max = if c.max > 0.0 { c.max } else { 1.0 };
    html! {
        div class="pulse__plot" data-js="pulse-plot" role="img" aria-label=(c.label) tabindex="0" {
            span class="pulse__max" aria-hidden="true" { (c.max_label) }
            svg class="pulse__svg" aria-hidden="true" {
                line class="pulse__grid" x1="0" x2="100%" y1="1" y2="1" {}
                line class="pulse__grid" x1="0" x2="100%" y1="50%" y2="50%" {}
                @for (t, _) in c.ticks {
                    line class="pulse__grid pulse__grid--v" x1=(pct(*t)) x2=(pct(*t)) y1="0" y2="100%" {}
                }
                @for m in c.marks {
                    line class={"pulse__mark pulse__mark--" (m.kind)} x1=(pct(m.time)) x2=(pct(m.time)) y1="0" y2="100%" {}
                }
                svg class="pulse__data" viewBox="0 0 1000 100" preserveAspectRatio="none" width="100%" height="100%" {
                    @for l in c.lines.iter().filter(|l| l.slot == "a") {
                        path class={"pulse__area pulse__area--" (l.slot)} d=(pulse_path(l.values, max, true)) {}
                    }
                    @for l in c.lines.iter().rev() {
                        path class={"pulse__line pulse__line--" (l.slot)} fill="none" vector-effect="non-scaling-stroke"
                            d=(pulse_path(l.values, max, false)) {}
                    }
                }
                line class="pulse__base" x1="0" x2="100%" y1="100%" y2="100%" {}
                line class="pulse__cross" data-js="pulse-cross" x1="0" x2="0" y1="0" y2="100%" visibility="hidden" {}
                @for m in c.marks {
                    circle class={"pulse__flag pulse__flag--" (m.kind)} cx=(pct(m.time)) cy="5" r="4" {
                        title { (m.title) }
                    }
                }
                @for l in c.lines {
                    circle class={"pulse__dot pulse__dot--" (l.slot)} data-js="pulse-dot" data-slot=(l.slot)
                        cx="0" cy="0" r="4" visibility="hidden" {}
                }
            }
            svg class="pulse__axis" aria-hidden="true" {
                @for (i, (t, label)) in c.ticks.iter().enumerate() {
                    text class="pulse__tick" x=(pct(*t)) y="13"
                        text-anchor=(if i == 0 && (*t - c.start) < span / 20 { "start" } else { "middle" }) { (label) }
                }
            }
        }
    }
}

pub struct FillChart<'a> {
    /// (time, percent full).
    pub points: &'a [(i64, f64)],
    pub start: i64,
    pub now: i64,
    /// Space to the right of `now` (seconds) for the projection.
    pub ahead: i64,
    /// When the line reaches 100 %, if it's rising.
    pub full_at: Option<i64>,
    /// (percent, class) horizontal lines.
    pub thresholds: &'a [(f64, &'a str)],
    pub label: &'a str,
    pub start_label: &'a str,
    pub now_label: &'a str,
}

/// Percent-full over time, with threshold lines and a dashed projection.
pub fn fill_chart(c: &FillChart) -> Markup {
    const W: f64 = 600.0;
    const H: f64 = 160.0;
    const L: f64 = 34.0;
    const B: f64 = 18.0;
    let end = c.now + c.ahead;
    if c.points.is_empty() || end <= c.start {
        return html! {};
    }
    let x = |t: i64| L + (t - c.start) as f64 / (end - c.start) as f64 * (W - L);
    let y = |p: f64| (H - B) - p.clamp(0.0, 100.0) / 100.0 * (H - B - 4.0);
    let line: Vec<(f64, f64)> = c.points.iter().map(|(t, p)| (x(*t), y(*p))).collect();
    let projection = c.full_at.and_then(|full| {
        let &(lt, lp) = c.points.last()?;
        // Straight line from the last reading to 100 % at `full`, cut at the right edge.
        let te = full.min(end);
        let pe = if full <= lt {
            100.0
        } else {
            lp + (100.0 - lp) * (te - lt) as f64 / (full - lt) as f64
        };
        Some(vec![(x(lt), y(lp)), (x(te), y(pe))])
    });
    html! {
        svg class="chart chart--fill" viewBox="0 0 600 160" role="img" aria-label=(c.label) {
            @for g in [0.0, 25.0, 50.0, 75.0, 100.0] {
                line class="chart__grid" x1=(fmt(L)) x2=(fmt(W)) y1=(fmt(y(g))) y2=(fmt(y(g)))
                    stroke="currentColor" stroke-opacity="0.15" {}
            }
            @for (g, label) in [(0.0, "0 %"), (50.0, "50 %"), (100.0, "100 %")] {
                text class="chart__label" x="0" y=(fmt(y(g) + 4.0)) font-size="10" fill="currentColor" { (label) }
            }
            @for (p, class) in c.thresholds {
                line class={"chart__threshold chart__threshold--" (class)}
                    x1=(fmt(L)) x2=(fmt(W)) y1=(fmt(y(*p))) y2=(fmt(y(*p)))
                    stroke="currentColor" stroke-dasharray="2 3" {}
            }
            line class="chart__now" x1=(fmt(x(c.now))) x2=(fmt(x(c.now))) y1="0" y2=(fmt(H - B))
                stroke="currentColor" stroke-opacity="0.3" {}
            polyline class="chart__line" fill="none" stroke="currentColor" stroke-width="2"
                points=(polyline(&line)) {}
            @if let Some(p) = &projection {
                polyline class="chart__projection" fill="none" stroke="currentColor"
                    stroke-dasharray="6 4" points=(polyline(p)) {}
            }
            text class="chart__label" x=(fmt(L)) y=(fmt(H - 4.0)) font-size="10" fill="currentColor" { (c.start_label) }
            text class="chart__label" x=(fmt(x(c.now))) y=(fmt(H - 4.0)) font-size="10" fill="currentColor"
                text-anchor="middle" { (c.now_label) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets() {
        let pts = [(0, 1.0), (10, 5.0), (100, 2.0), (150, 3.0)];
        assert_eq!(bucket_max(&pts, 100), [(0, 5.0), (100, 3.0)]);
        assert_eq!(bucket_last(&pts, 100), [(0, 5.0), (100, 3.0)]);
        assert_eq!(bucket_last(&pts[..1], 100), [(0, 1.0)]);
    }

    #[test]
    fn svg_output() {
        let s = sparkline(
            &[(0, 30.0), (50, 35.0), (100, 32.0)],
            0,
            100,
            "Temperature",
            "t",
        )
        .into_string();
        assert!(
            s.starts_with("<svg class=\"sparkline t\"") && s.contains("polyline"),
            "{s}"
        );
        assert!(
            sparkline(&[(0, 1.0)], 0, 100, "x", "t")
                .into_string()
                .is_empty()
        );
        let c = fill_chart(&FillChart {
            points: &[(0, 60.0), (50, 65.0), (100, 67.0)],
            start: 0,
            now: 100,
            ahead: 50,
            full_at: Some(400),
            thresholds: &[(80.0, "warning"), (90.0, "critical")],
            label: "Pool tank",
            start_label: "Aug",
            now_label: "today",
        })
        .into_string();
        assert!(
            c.contains("chart__projection") && c.contains("chart__threshold--critical"),
            "{c}"
        );
        assert!(c.contains("aria-label=\"Pool tank\""));
    }
}
