//! Server-side SVG charts for the dashboard: sparklines and a fill chart with thresholds
//! and a projection. Plain SVG with class names; colors and strokes come from the CSS.

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
