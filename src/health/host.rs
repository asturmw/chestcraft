//! The machine itself: uptime, load, memory, ZFS ARC, CPU temperature, root filesystem,
//! and the activity counters (CPU time, network and disk bytes) the dashboard turns into
//! rates. Everything comes from /proc and /sys, readable without privileges.

use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Host {
    pub uptime_secs: Option<u64>,
    pub load: Option<[f64; 3]>,
    pub cpus: Option<usize>,
    pub mem_total: Option<u64>,
    pub mem_available: Option<u64>,
    pub arc: Option<Arc>,
    pub cpu_temp: Option<f64>,
    pub root_fs: Option<FsUse>,
    /// Running totals; only the difference between two readings means something.
    #[serde(skip)]
    pub counters: Counters,
}

/// Running totals since boot, read every minute; the monitor turns two readings into
/// rates (`history::Activity`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Counters {
    /// The `cpu` line of /proc/stat: (busy, waiting for I/O, total) in clock ticks.
    pub cpu: Option<(u64, u64, u64)>,
    /// Physical network interfaces: (received, sent) bytes.
    pub net: Option<(u64, u64)>,
    /// The machine's disks: (read, written) bytes.
    pub disk: Option<(u64, u64)>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
pub struct Arc {
    pub size: u64,
    pub max: u64,
    pub hits: u64,
    pub misses: u64,
    /// Hit rate since the previous reading, else since boot (percent).
    pub hit_rate: Option<f64>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
pub struct FsUse {
    pub total: u64,
    pub used: u64,
    pub avail: u64,
}

impl FsUse {
    /// Percent used, as `df` counts it (used / (used + available to users)).
    pub fn percent(&self) -> u64 {
        let base = self.used + self.avail;
        if base == 0 {
            0
        } else {
            (self.used * 100).div_ceil(base)
        }
    }
}

pub fn parse_uptime(text: &str) -> Option<u64> {
    text.split_whitespace()
        .next()?
        .parse::<f64>()
        .ok()
        .map(|s| s as u64)
}

pub fn parse_loadavg(text: &str) -> Option<[f64; 3]> {
    let mut w = text.split_whitespace().map(|v| v.parse::<f64>().ok());
    Some([w.next()??, w.next()??, w.next()??])
}

/// (MemTotal, MemAvailable) in bytes.
pub fn parse_meminfo(text: &str) -> (Option<u64>, Option<u64>) {
    let field = |name: &str| {
        text.lines().find_map(|l| {
            let rest = l.strip_prefix(name)?.strip_prefix(':')?;
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            Some(kb * 1024)
        })
    };
    (field("MemTotal"), field("MemAvailable"))
}

/// `/proc/spl/kstat/zfs/arcstats`: `name type data` rows.
pub fn parse_arcstats(text: &str) -> Option<Arc> {
    let field = |name: &str| {
        text.lines().find_map(|l| {
            let mut w = l.split_whitespace();
            (w.next()? == name).then_some(())?;
            w.nth(1)?.parse::<u64>().ok()
        })
    };
    let (hits, misses) = (field("hits")?, field("misses")?);
    Some(Arc {
        size: field("size")?,
        max: field("c_max").unwrap_or(0),
        hits,
        misses,
        hit_rate: rate(hits, misses),
    })
}

fn rate(hits: u64, misses: u64) -> Option<f64> {
    let total = hits + misses;
    (total > 0).then(|| hits as f64 * 100.0 / total as f64)
}

impl Arc {
    /// Use the hit rate since `prev` (hits, misses) when the counters moved forward.
    pub fn since(mut self, prev: Option<(u64, u64)>) -> Arc {
        if let Some((h, m)) = prev
            && self.hits >= h
            && self.misses >= m
            && let Some(r) = rate(self.hits - h, self.misses - m)
        {
            self.hit_rate = Some(r);
        }
        self
    }
}

/// The `cpu` line of /proc/stat: (busy, iowait, total) ticks. Guest time is already part
/// of user time, so it isn't added again.
pub fn parse_stat(text: &str) -> Option<(u64, u64, u64)> {
    let line = text.lines().find(|l| l.starts_with("cpu "))?;
    let v: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .take(8)
        .map(|f| f.parse().ok())
        .collect::<Option<_>>()?;
    if v.len() < 5 {
        return None;
    }
    let total: u64 = v.iter().sum();
    let (idle, iowait) = (v[3], v[4]);
    Some((total - idle - iowait, iowait, total))
}

/// /proc/net/dev: (received, sent) bytes summed over the interfaces `keep` accepts. None
/// when it accepts none.
pub fn parse_net_dev(text: &str, keep: impl Fn(&str) -> bool) -> Option<(u64, u64)> {
    let mut sum: Option<(u64, u64)> = None;
    for line in text.lines() {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if !keep(name) {
            continue;
        }
        let f: Vec<&str> = rest.split_whitespace().collect();
        let (Some(rx), Some(tx)) = (
            f.first().and_then(|v| v.parse::<u64>().ok()),
            f.get(8).and_then(|v| v.parse::<u64>().ok()),
        ) else {
            continue;
        };
        let s = sum.get_or_insert((0, 0));
        s.0 += rx;
        s.1 += tx;
    }
    sum
}

/// /proc/diskstats: (read, written) bytes of the named block devices (sectors are 512
/// bytes there, whatever the disk's own sector size). None when none of them is listed.
pub fn parse_diskstats(text: &str, names: &[&str]) -> Option<(u64, u64)> {
    let mut sum: Option<(u64, u64)> = None;
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 10 || !names.contains(&f[2]) {
            continue;
        }
        let (Ok(r), Ok(w)) = (f[5].parse::<u64>(), f[9].parse::<u64>()) else {
            continue;
        };
        let s = sum.get_or_insert((0, 0));
        s.0 += r * 512;
        s.1 += w * 512;
    }
    sum
}

/// Network interfaces backed by hardware (`/sys/class/net/<if>/device`): bridges, bonds,
/// VPNs and container links carry the same bytes again and are left out.
fn physical_interface(sys: &Path, name: &str) -> bool {
    name != "lo" && !name.contains('/') && sys.join("class/net").join(name).join("device").exists()
}

/// Bytes the given disks (block device names, `sda`, `nvme0n1`) read and wrote since boot.
pub fn disk_bytes(proc: &Path, disks: &[&str]) -> Option<(u64, u64)> {
    let text = std::fs::read_to_string(proc.join("diskstats")).ok()?;
    parse_diskstats(&text, disks)
}

/// CPU sensors by hwmon driver name, best first, with the preferred label (None = first
/// temperature of the device).
const CPU_SENSORS: &[(&str, Option<&str>)] = &[
    ("k10temp", Some("Tdie")),
    ("k10temp", Some("Tctl")),
    ("zenpower", Some("Tdie")),
    ("coretemp", Some("Package id 0")),
    ("cpu_thermal", None),
    ("soc_thermal", None),
    ("k10temp", None),
    ("coretemp", None),
];

/// One hwmon device: driver name and its (label, °C) temperatures.
pub type HwmonDevice = (String, Vec<(Option<String>, f64)>);

pub fn read_hwmon(sys: &Path) -> Vec<HwmonDevice> {
    let mut out = Vec::new();
    let Ok(dir) = std::fs::read_dir(sys.join("class/hwmon")) else {
        return out;
    };
    let mut entries: Vec<_> = dir.flatten().map(|e| e.path()).collect();
    entries.sort();
    for dev in entries {
        let Ok(name) = std::fs::read_to_string(dev.join("name")) else {
            continue;
        };
        let mut temps = Vec::new();
        for i in 1..=32 {
            let Ok(v) = std::fs::read_to_string(dev.join(format!("temp{i}_input"))) else {
                continue;
            };
            let Ok(milli) = v.trim().parse::<i64>() else {
                continue;
            };
            let label = std::fs::read_to_string(dev.join(format!("temp{i}_label")))
                .ok()
                .map(|l| l.trim().to_string());
            temps.push((label, milli as f64 / 1000.0));
        }
        out.push((name.trim().to_string(), temps));
    }
    out
}

pub fn cpu_temp(devices: &[HwmonDevice]) -> Option<f64> {
    CPU_SENSORS.iter().find_map(|(driver, label)| {
        devices
            .iter()
            .filter(|(name, _)| name == driver)
            .find_map(|(_, temps)| match label {
                Some(l) => temps
                    .iter()
                    .find(|(tl, _)| tl.as_deref() == Some(*l))
                    .map(|t| t.1),
                None => temps.first().map(|t| t.1),
            })
    })
}

pub fn fs_use(path: &Path) -> Option<FsUse> {
    let s = rustix::fs::statvfs(path).ok()?;
    let frag = s.f_frsize;
    let total = s.f_blocks * frag;
    let free = s.f_bfree * frag;
    Some(FsUse {
        total,
        used: total.saturating_sub(free),
        avail: s.f_bavail * frag,
    })
}

/// Read everything; `arc_prev` is the (hits, misses) of the previous reading.
pub fn read(proc: &Path, sys: &Path, arc_prev: Option<(u64, u64)>) -> Host {
    let file = |p: &Path| std::fs::read_to_string(p).ok();
    let (mem_total, mem_available) = file(&proc.join("meminfo"))
        .map(|t| parse_meminfo(&t))
        .unwrap_or_default();
    Host {
        uptime_secs: file(&proc.join("uptime")).and_then(|t| parse_uptime(&t)),
        load: file(&proc.join("loadavg")).and_then(|t| parse_loadavg(&t)),
        cpus: std::thread::available_parallelism().ok().map(|n| n.get()),
        mem_total,
        mem_available,
        arc: file(&proc.join("spl/kstat/zfs/arcstats"))
            .and_then(|t| parse_arcstats(&t))
            .map(|a| a.since(arc_prev)),
        cpu_temp: cpu_temp(&read_hwmon(sys)),
        root_fs: fs_use(Path::new("/")),
        counters: Counters {
            cpu: file(&proc.join("stat")).and_then(|t| parse_stat(&t)),
            net: file(&proc.join("net/dev"))
                .and_then(|t| parse_net_dev(&t, |n| physical_interface(sys, n))),
            // Filled in by `health::gather` once the disks are known.
            disk: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_files() {
        assert_eq!(parse_uptime("756194.54 9028455.30\n"), Some(756_194));
        assert_eq!(
            parse_loadavg("0.00 0.03 1.50 1/419 188693\n"),
            Some([0.0, 0.03, 1.5])
        );
        let (t, a) = parse_meminfo(
            "MemTotal:       32145460 kB\nMemFree:         1742784 kB\nMemAvailable:    2488536 kB\n",
        );
        assert_eq!((t, a), (Some(32_145_460 * 1024), Some(2_488_536 * 1024)));
        let arc = parse_arcstats(
            "13 1 0x01 147 39984 1 2\nname type data\nhits 4 124422998\nmisses 4 7530466\nc 4 28622308296\nc_max 4 31843209216\nsize 4 28222672704\n",
        )
        .unwrap();
        assert_eq!(arc.size, 28_222_672_704);
        assert_eq!(arc.max, 31_843_209_216);
        assert!((arc.hit_rate.unwrap() - 94.29).abs() < 0.01);
        let later = Arc {
            hits: arc.hits + 90,
            misses: arc.misses + 10,
            ..arc
        }
        .since(Some((arc.hits, arc.misses)));
        assert!((later.hit_rate.unwrap() - 90.0).abs() < 1e-9);
        // Counters reset (module reload): keep the lifetime rate.
        assert_eq!(arc.since(Some((u64::MAX, 0))).hit_rate, arc.hit_rate);
    }

    #[test]
    fn cpu_sensor_choice() {
        let dev = |n: &str, t: &[(Option<&str>, f64)]| {
            (
                n.to_string(),
                t.iter()
                    .map(|(l, v)| (l.map(String::from), *v))
                    .collect::<Vec<_>>(),
            )
        };
        let devices = vec![
            dev("nvme", &[(Some("Composite"), 45.85)]),
            dev("k10temp", &[(Some("Tctl"), 53.0), (Some("Tccd1"), 47.0)]),
            dev("amdgpu", &[(Some("edge"), 20.0)]),
        ];
        assert_eq!(cpu_temp(&devices), Some(53.0));
        let intel = vec![dev(
            "coretemp",
            &[(Some("Core 0"), 40.0), (Some("Package id 0"), 44.0)],
        )];
        assert_eq!(cpu_temp(&intel), Some(44.0));
        assert_eq!(cpu_temp(&[dev("cpu_thermal", &[(None, 61.2)])]), Some(61.2));
        assert_eq!(cpu_temp(&devices[2..]), None);
    }

    #[test]
    fn activity_counters() {
        assert_eq!(
            parse_stat("cpu  100 5 50 1000 20 3 2 1 0 0\ncpu0 1 2 3 4 5 6 7 8 0 0\n"),
            Some((161, 20, 1181))
        );
        assert_eq!(parse_stat("intr 1 2 3\n"), None);
        let dev = "Inter-|   Receive                                                |  Transmit\n \
             face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n    \
             lo: 5000 10 0 0 0 0 0 0 5000 10 0 0 0 0 0 0\n  \
             eno1: 1000 10 0 0 0 0 0 0 300 5 0 0 0 0 0 0\n\
             vmbr0: 900 9 0 0 0 0 0 0 250 4 0 0 0 0 0 0\n  \
             eno2: 24 1 0 0 0 0 0 0 6 1 0 0 0 0 0 0\n";
        assert_eq!(
            parse_net_dev(dev, |n| n.starts_with("eno")),
            Some((1024, 306))
        );
        assert_eq!(parse_net_dev(dev, |_| false), None);
        let stats = "   8       0 sda 100 0 2048 50 200 0 4096 80 0 100 130\n   \
             8       1 sda1 90 0 2000 40 190 0 4000 70 0 90 110\n 259       0 nvme0n1 10 0 8 1 20 0 16 2 0 3 3\n";
        assert_eq!(
            parse_diskstats(stats, &["sda", "nvme0n1"]),
            Some(((2048 + 8) * 512, (4096 + 16) * 512))
        );
        assert_eq!(parse_diskstats(stats, &["sdb"]), None);
    }

    #[test]
    fn reads_this_machine() {
        let h = read(Path::new("/proc"), Path::new("/sys"), None);
        assert!(h.uptime_secs.is_some() && h.load.is_some() && h.mem_total.is_some());
        assert!(h.counters.cpu.is_some());
        let fs = h.root_fs.unwrap();
        assert!(fs.total > 0 && fs.percent() <= 100);
    }
}
