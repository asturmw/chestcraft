//! ZFS pools from `zpool status -j --json-int` and `zpool list -j --json-int` (OpenZFS 2.3+).
//! Any layout: stripes, mirrors, raidz1–3, draid, special/log/dedup vdevs, cache and spares.

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Pool {
    pub name: String,
    /// ONLINE, DEGRADED, FAULTED, OFFLINE, UNAVAIL, REMOVED, SUSPENDED.
    pub state: String,
    pub size: u64,
    pub alloc: u64,
    pub free: u64,
    /// Percent; None when the pool doesn't report it.
    pub frag: Option<u64>,
    /// Percent used.
    pub capacity: u64,
    /// Top-level vdevs (data, then special/dedup/log by their `class`).
    pub vdevs: Vec<Vdev>,
    pub spares: Vec<Vdev>,
    pub cache: Vec<Vdev>,
    pub scan: Option<Scan>,
    pub expand: Option<Expand>,
    /// Files with permanent errors.
    pub data_errors: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Vdev {
    pub name: String,
    /// disk, file, mirror, raidz, draid, replacing, spare, …
    pub kind: String,
    /// normal, special, dedup, log, …
    pub class: String,
    pub state: String,
    pub path: Option<String>,
    pub guid: String,
    pub read: u64,
    pub write: u64,
    pub cksum: u64,
    pub children: Vec<Vdev>,
    /// The block device (`sda`) a disk vdev lives on, filled in by [`crate::health::disks`].
    pub disk: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Scan {
    /// SCRUB, RESILVER or ERRORSCRUB.
    pub function: String,
    /// SCANNING, FINISHED or CANCELED.
    pub state: String,
    pub start: i64,
    pub end: Option<i64>,
    pub to_examine: u64,
    pub issued: u64,
    pub errors: u64,
    pub paused: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Expand {
    pub vdev: String,
    pub state: String,
    pub start: i64,
    pub end: Option<i64>,
    pub to_reflow: u64,
    pub reflowed: u64,
}

impl Scan {
    pub fn running(&self) -> bool {
        self.state == "SCANNING"
    }

    pub fn is_resilver(&self) -> bool {
        self.function == "RESILVER"
    }

    pub fn is_scrub(&self) -> bool {
        self.function == "SCRUB"
    }

    /// Progress in percent (0–100).
    pub fn percent(&self) -> f64 {
        if self.to_examine == 0 {
            return if self.running() { 0.0 } else { 100.0 };
        }
        (self.issued as f64 * 100.0 / self.to_examine as f64).clamp(0.0, 100.0)
    }
}

impl Expand {
    pub fn running(&self) -> bool {
        self.state == "SCANNING"
    }

    pub fn percent(&self) -> f64 {
        if self.to_reflow == 0 {
            return 100.0;
        }
        (self.reflowed as f64 * 100.0 / self.to_reflow as f64).clamp(0.0, 100.0)
    }
}

impl Vdev {
    /// This vdev and everything below it.
    pub fn walk(&self) -> Vec<&Vdev> {
        let mut out = vec![self];
        for c in &self.children {
            out.extend(c.walk());
        }
        out
    }

    pub fn is_leaf(&self) -> bool {
        self.children.is_empty()
    }
}

impl Pool {
    /// Every vdev of the pool, depth first (data vdevs, then spares and cache).
    pub fn all_vdevs(&self) -> Vec<&Vdev> {
        self.vdevs
            .iter()
            .chain(&self.spares)
            .chain(&self.cache)
            .flat_map(Vdev::walk)
            .collect()
    }
}

/// A number from `--json-int` output, or a numeric string from older versions.
pub(crate) fn num(v: Option<&Value>) -> Option<u64> {
    match v? {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn int(v: Option<&Value>) -> i64 {
    num(v).map(|n| n as i64).unwrap_or(0)
}

fn text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// A property value from `zpool list -j` / `zfs list -j`: `properties.<name>.value`.
pub(crate) fn prop<'a>(obj: &'a Value, name: &str) -> Option<&'a Value> {
    obj.get("properties")?.get(name)?.get("value")
}

fn vdev(v: &Value) -> Vdev {
    let children = v
        .get("vdevs")
        .and_then(Value::as_object)
        .map(|m| m.values().map(vdev).collect())
        .unwrap_or_default();
    Vdev {
        name: text(v.get("name")),
        kind: text(v.get("vdev_type")),
        class: v
            .get("class")
            .and_then(Value::as_str)
            .unwrap_or("normal")
            .to_string(),
        state: text(v.get("state")),
        path: v.get("path").and_then(Value::as_str).map(String::from),
        guid: text(v.get("guid")),
        read: num(v.get("read_errors")).unwrap_or(0),
        write: num(v.get("write_errors")).unwrap_or(0),
        cksum: num(v.get("checksum_errors")).unwrap_or(0),
        children,
        disk: None,
    }
}

fn vdev_map(v: Option<&Value>) -> Vec<Vdev> {
    v.and_then(Value::as_object)
        .map(|m| m.values().map(vdev).collect())
        .unwrap_or_default()
}

fn parse_json(json: &[u8], what: &str) -> Result<Value, String> {
    let trimmed = json.trim_ascii();
    // Without pools, some versions print "no pools available" instead of JSON.
    if trimmed.is_empty() || trimmed.starts_with(b"no pools") {
        return Ok(Value::Object(Default::default()));
    }
    serde_json::from_slice(trimmed).map_err(|e| format!("{what}: unexpected output ({e})"))
}

/// Parse `zpool status -j --json-int`, then add sizes from `zpool list -j --json-int -o
/// name,size,allocated,free,fragmentation,capacity,health`. Pools are sorted by name.
pub fn parse(status_json: &[u8], list_json: &[u8]) -> Result<Vec<Pool>, String> {
    let status = parse_json(status_json, "zpool status")?;
    let list = parse_json(list_json, "zpool list")?;
    let empty = serde_json::Map::new();
    let pools = status
        .get("pools")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let listed = list.get("pools").and_then(Value::as_object);
    let mut out = Vec::new();
    for (name, p) in pools {
        let l = listed.and_then(|m| m.get(name));
        let lp = |key: &str| l.and_then(|l| prop(l, key));
        // The root vdev is named like the pool; its children are the top-level vdevs.
        let root = p
            .get("vdevs")
            .and_then(Value::as_object)
            .and_then(|m| m.values().next());
        let scan = p.get("scan_stats").filter(|s| s.is_object()).and_then(|s| {
            let function = text(s.get("function"));
            let state = text(s.get("state"));
            (!function.is_empty() && function != "NONE" && state != "NONE").then(|| Scan {
                function,
                state,
                start: int(s.get("start_time")),
                end: Some(int(s.get("end_time"))).filter(|t| *t > 0),
                to_examine: num(s.get("to_examine")).unwrap_or(0),
                issued: num(s.get("issued"))
                    .or_else(|| num(s.get("examined")))
                    .unwrap_or(0),
                errors: num(s.get("errors")).unwrap_or(0),
                paused: num(s.get("scrub_pause")).unwrap_or(0) != 0,
            })
        });
        let expand = p
            .get("raidz_expand_stats")
            .filter(|s| s.is_object())
            .map(|s| Expand {
                vdev: text(s.get("name")),
                state: text(s.get("state")),
                start: int(s.get("start_time")),
                end: Some(int(s.get("end_time"))).filter(|t| *t > 0),
                to_reflow: num(s.get("to_reflow")).unwrap_or(0),
                reflowed: num(s.get("reflowed")).unwrap_or(0),
            });
        let size = num(lp("size")).unwrap_or(0);
        let alloc = num(lp("allocated")).unwrap_or(0);
        out.push(Pool {
            name: name.clone(),
            state: text(p.get("state")),
            size,
            alloc,
            free: num(lp("free")).unwrap_or(size.saturating_sub(alloc)),
            frag: num(lp("fragmentation")),
            capacity: num(lp("capacity")).unwrap_or((alloc * 100).checked_div(size).unwrap_or(0)),
            vdevs: root.map(|r| vdev(r).children).unwrap_or_default(),
            spares: vdev_map(p.get("spares")),
            cache: vdev_map(p.get("l2cache")),
            scan,
            expand,
            data_errors: num(p.get("error_count")).unwrap_or(0),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!("{}/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    #[test]
    fn healthy_raidz1() {
        let pools = parse(
            &fixture("zpool-status-raidz1.json"),
            &fixture("zpool-list.json"),
        )
        .unwrap();
        assert_eq!(pools.len(), 1);
        let p = &pools[0];
        assert_eq!(p.name, "tank");
        assert_eq!(p.state, "ONLINE");
        assert_eq!((p.capacity, p.frag), (67, Some(4)));
        assert_eq!(p.size, 11_991_548_690_432);
        assert_eq!(p.vdevs.len(), 1);
        let raidz = &p.vdevs[0];
        assert_eq!(
            (raidz.name.as_str(), raidz.kind.as_str()),
            ("raidz1-0", "raidz")
        );
        assert_eq!(raidz.children.len(), 4);
        assert!(
            raidz
                .children
                .iter()
                .all(|d| d.kind == "disk" && d.path.is_some())
        );
        let scan = p.scan.as_ref().unwrap();
        assert!(scan.is_scrub() && !scan.running());
        assert_eq!(scan.errors, 0);
        assert_eq!(scan.end, Some(1_791_691_103));
        let ex = p.expand.as_ref().unwrap();
        assert!(!ex.running());
        assert_eq!(p.data_errors, 0);
        assert_eq!(p.all_vdevs().len(), 5);
    }

    #[test]
    fn degraded_mirror_with_special_log_cache_and_spare() {
        let pools = parse(
            &fixture("zpool-status-degraded.json"),
            &fixture("zpool-list-two.json"),
        )
        .unwrap();
        assert_eq!(
            pools.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            ["backup", "fast"]
        );
        let fast = &pools[1];
        assert_eq!(fast.state, "DEGRADED");
        assert_eq!(fast.capacity, 83);
        let classes: Vec<_> = fast.vdevs.iter().map(|v| v.class.as_str()).collect();
        assert_eq!(classes, ["normal", "special", "log"]);
        let mirror = &fast.vdevs[0];
        assert_eq!(mirror.state, "DEGRADED");
        let bad = mirror
            .children
            .iter()
            .find(|d| d.state == "FAULTED")
            .unwrap();
        assert_eq!((bad.read, bad.write, bad.cksum), (3, 0, 12));
        assert_eq!(fast.spares.len(), 1);
        assert_eq!(fast.spares[0].state, "AVAIL");
        assert_eq!(fast.cache.len(), 1);
        let scan = fast.scan.as_ref().unwrap();
        assert!(scan.is_resilver() && scan.running());
        assert!((scan.percent() - 37.0).abs() < 0.5, "{}", scan.percent());
        assert_eq!(fast.data_errors, 2);
        // A pool that was never scrubbed has no scan.
        assert!(pools[0].scan.is_none());
    }

    #[test]
    fn no_pools_and_garbage() {
        assert!(parse(b"no pools available\n", b"").unwrap().is_empty());
        assert!(
            parse(br#"{"output_version":{},"pools":{}}"#, b"{}")
                .unwrap()
                .is_empty()
        );
        assert!(parse(b"<html>", b"").is_err());
    }
}
