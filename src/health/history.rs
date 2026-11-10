//! 90 days of hourly readings for the dashboard charts: pool fill, disk temperatures,
//! reallocated sectors and the machine's activity (CPU, memory, network, disks). One small
//! record per hour in the state database; the activity also per minute for the last day,
//! in memory (`web::monitor`).

use super::host::{Counters, Host};
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
    /// Mean activity of the hour before this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<Activity>,
}

/// What the machine was doing over a stretch of time. Fields the machine can't read stay
/// None.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct Activity {
    /// Percent of CPU time busy, and waiting for disks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iowait: Option<f64>,
    /// Memory in use (total − available) and the ZFS ARC, bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arc: Option<f64>,
    /// Physical network interfaces, bytes per second.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net_rx: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net_tx: Option<f64>,
    /// The machine's disks, bytes per second.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_read: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_write: Option<f64>,
}

/// Per-second rate of a counter pair; None when a counter went backwards (reboot, reset).
fn rates(prev: Option<(u64, u64)>, cur: Option<(u64, u64)>, dt: f64) -> (Option<f64>, Option<f64>) {
    match (prev, cur) {
        (Some(p), Some(c)) if c.0 >= p.0 && c.1 >= p.1 => {
            (Some((c.0 - p.0) as f64 / dt), Some((c.1 - p.1) as f64 / dt))
        }
        _ => (None, None),
    }
}

impl Activity {
    /// The machine now: memory from `host`, rates from the counters `dt` seconds ago (none
    /// on the first reading).
    pub fn reading(host: &Host, prev: Option<(&Counters, i64)>) -> Activity {
        let mut a = Activity {
            mem: host
                .mem_total
                .zip(host.mem_available)
                .map(|(t, av)| t.saturating_sub(av) as f64),
            arc: host.arc.map(|a| a.size as f64),
            ..Activity::default()
        };
        let Some((p, dt)) = prev.filter(|(_, dt)| *dt > 0) else {
            return a;
        };
        let cur = &host.counters;
        let dt = dt as f64;
        if let (Some((b0, w0, t0)), Some((b1, w1, t1))) = (p.cpu, cur.cpu)
            && t1 > t0
            && b1 >= b0
            && w1 >= w0
        {
            let total = (t1 - t0) as f64;
            a.cpu = Some(((b1 - b0) as f64 * 100.0 / total).min(100.0));
            a.iowait = Some(((w1 - w0) as f64 * 100.0 / total).min(100.0));
        }
        (a.net_rx, a.net_tx) = rates(p.net, cur.net, dt);
        (a.disk_read, a.disk_write) = rates(p.disk, cur.disk, dt);
        a
    }

    /// Mean of each field over the readings that have it; None without readings.
    pub fn mean<'a>(items: impl IntoIterator<Item = &'a Activity>) -> Option<Activity> {
        let mut sum = [(0.0f64, 0u32); 8];
        let mut any = false;
        for a in items {
            any = true;
            for (slot, v) in sum.iter_mut().zip(a.fields()) {
                if let Some(v) = v {
                    slot.0 += v;
                    slot.1 += 1;
                }
            }
        }
        let m = sum.map(|(s, n)| (n > 0).then(|| s / n as f64));
        any.then(|| Activity::from_fields(m))
    }

    fn fields(&self) -> [Option<f64>; 8] {
        [
            self.cpu,
            self.iowait,
            self.mem,
            self.arc,
            self.net_rx,
            self.net_tx,
            self.disk_read,
            self.disk_write,
        ]
    }

    fn from_fields(f: [Option<f64>; 8]) -> Activity {
        Activity {
            cpu: f[0],
            iowait: f[1],
            mem: f[2],
            arc: f[3],
            net_rx: f[4],
            net_tx: f[5],
            disk_read: f[6],
            disk_write: f[7],
        }
    }
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
        self.pools.is_empty()
            && self.temps.is_empty()
            && self.realloc.is_empty()
            && self.activity.is_none()
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
    fn activity_rates() {
        let mut host = Host {
            mem_total: Some(16_000),
            mem_available: Some(6_000),
            ..Host::default()
        };
        host.counters = Counters {
            cpu: Some((100, 10, 1000)),
            net: Some((1_000, 500)),
            disk: Some((0, 0)),
        };
        let first = Activity::reading(&host, None);
        assert_eq!((first.mem, first.cpu), (Some(10_000.0), None));
        let prev = host.counters;
        host.counters = Counters {
            cpu: Some((130, 30, 1100)),
            net: Some((7_000, 1_100)),
            disk: Some((60_000, 120_000)),
        };
        let a = Activity::reading(&host, Some((&prev, 60)));
        assert_eq!((a.cpu, a.iowait), (Some(30.0), Some(20.0)));
        assert_eq!((a.net_rx, a.net_tx), (Some(100.0), Some(10.0)));
        assert_eq!((a.disk_read, a.disk_write), (Some(1_000.0), Some(2_000.0)));
        // A reboot resets the counters: no rate rather than a huge one.
        let reset = Counters {
            net: Some((u64::MAX, 0)),
            ..host.counters
        };
        let b = Activity::reading(&host, Some((&reset, 60)));
        assert_eq!((b.net_rx, b.disk_read), (None, Some(0.0)));
        let m = Activity::mean([&a, &first]).unwrap();
        assert_eq!((m.cpu, m.mem), (Some(30.0), Some(10_000.0)));
        assert_eq!(Activity::mean([]), None);
        // Old records without the field still load.
        let old: Sample = serde_json::from_str(r#"{"pools":{},"temps":{"S1":40}}"#).unwrap();
        assert_eq!(old.activity, None);
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
