//! 90 days of hourly readings for the dashboard charts: pool fill, disk temperatures and
//! reallocated sectors. One small record per hour in the state database.

use super::{Probe, Snapshot};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const KEEP_SECS: i64 = 90 * 86_400;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Sample {
    /// Pool → (allocated, size) in bytes.
    #[serde(default)]
    pub pools: BTreeMap<String, (u64, u64)>,
    /// Disk id → °C.
    #[serde(default)]
    pub temps: BTreeMap<String, i32>,
    /// Disk id → reallocated sectors (ATA) or grown defects (SCSI).
    #[serde(default)]
    pub realloc: BTreeMap<String, u64>,
}

/// Start of the hour `t` is in.
pub fn hour_of(t: i64) -> i64 {
    t - t.rem_euclid(3600)
}

impl Sample {
    pub fn from_snapshot(s: &Snapshot) -> Sample {
        let mut out = Sample::default();
        if let Probe::Ok(pools) = &s.pools {
            for p in pools {
                out.pools.insert(p.name.clone(), (p.alloc, p.size));
            }
        }
        for d in &s.disks {
            if let Some(smart) = &d.smart {
                if let Some(t) = smart.temp {
                    out.temps.insert(d.id(), t);
                }
                if let Some(r) = smart.realloc {
                    out.realloc.insert(d.id(), r);
                }
            }
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.pools.is_empty() && self.temps.is_empty() && self.realloc.is_empty()
    }
}

/// `(time, value)` points of one series, picked from the samples.
pub fn series(samples: &[(i64, Sample)], pick: impl Fn(&Sample) -> Option<f64>) -> Vec<(i64, f64)> {
    samples
        .iter()
        .filter_map(|(t, s)| pick(s).map(|v| (*t, v)))
        .collect()
}

/// When a pool at `alloc` of `size` will be full, from a least-squares line through the
/// points of the last 30 days. None without a week of data or when it isn't filling up.
pub fn full_at(points: &[(i64, f64)], size: f64, now: i64) -> Option<i64> {
    let recent: Vec<(f64, f64)> = points
        .iter()
        .filter(|(t, _)| now - t <= 30 * 86_400)
        .map(|(t, v)| ((*t - now) as f64, *v))
        .collect();
    let (first, last) = (recent.first()?, recent.last()?);
    if last.0 - first.0 < 7.0 * 86_400.0 {
        return None;
    }
    let n = recent.len() as f64;
    let mx = recent.iter().map(|p| p.0).sum::<f64>() / n;
    let my = recent.iter().map(|p| p.1).sum::<f64>() / n;
    let sxx: f64 = recent.iter().map(|p| (p.0 - mx).powi(2)).sum();
    let sxy: f64 = recent.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
    if sxx <= 0.0 {
        return None;
    }
    let slope = sxy / sxx; // bytes per second
    // Less than ~1 MB a day is "not filling up".
    if slope <= 1e6 / 86_400.0 {
        return None;
    }
    let current = my + slope * (0.0 - mx);
    let secs = (size - current) / slope;
    (secs.is_finite() && secs > 0.0 && secs < 100.0 * 365.0 * 86_400.0).then(|| now + secs as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection() {
        let now = 100 * 86_400;
        // 10 GB a day for 20 days, 1 TB left.
        let pts: Vec<(i64, f64)> = (0..=20)
            .map(|d| (now - (20 - d) * 86_400, 5e12 + d as f64 * 1e10))
            .collect();
        let full = full_at(&pts, 5.2e12 + 1e12, now).unwrap();
        assert!(((full - now) as f64 / 86_400.0 - 100.0).abs() < 0.5);
        // Shrinking or flat: none. Three days of data: none.
        let flat: Vec<(i64, f64)> = pts.iter().map(|(t, _)| (*t, 5e12)).collect();
        assert_eq!(full_at(&flat, 6e12, now), None);
        assert_eq!(full_at(&pts[17..], 6.2e12, now), None);
        assert_eq!(full_at(&[], 1.0, now), None);
    }

    #[test]
    fn hours_and_series() {
        assert_eq!(hour_of(7205), 7200);
        let mut a = Sample::default();
        a.temps.insert("S1".into(), 40);
        let b = Sample::default();
        let pts = series(&[(0, a), (3600, b)], |s| {
            s.temps.get("S1").map(|t| *t as f64)
        });
        assert_eq!(pts, [(0, 40.0)]);
    }
}
