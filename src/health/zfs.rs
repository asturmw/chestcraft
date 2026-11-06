//! Datasets from `zfs list -j --json-int`, with snapshot counts and the newest snapshot.

use super::zpool::{num, prop};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;

pub const LIST_ARGS: &[&str] = &[
    "list",
    "-j",
    "--json-int",
    "-t",
    "filesystem,volume",
    "-o",
    "name,used,available,quota,mountpoint,usedbysnapshots,type",
];
pub const SNAPSHOT_ARGS: &[&str] = &[
    "list",
    "-j",
    "--json-int",
    "-t",
    "snapshot",
    "-o",
    "name,creation",
];

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Dataset {
    pub name: String,
    pub pool: String,
    /// filesystem or volume.
    pub kind: String,
    pub used: u64,
    pub avail: u64,
    /// 0 = none.
    pub quota: u64,
    /// None for `none`, `legacy` and volumes.
    pub mountpoint: Option<String>,
    /// Space only snapshots hold.
    pub snap_used: u64,
    pub snapshots: u64,
    /// Creation time of the newest snapshot.
    pub newest_snapshot: Option<i64>,
}

impl Dataset {
    /// Percent of the quota in use.
    pub fn quota_percent(&self) -> Option<u64> {
        (self.quota > 0).then(|| self.used.saturating_mul(100) / self.quota)
    }
}

fn parse_json(json: &[u8], what: &str) -> Result<Value, String> {
    let t = json.trim_ascii();
    if t.is_empty() || t.starts_with(b"no datasets") {
        return Ok(Value::Object(Default::default()));
    }
    serde_json::from_slice(t).map_err(|e| format!("{what}: unexpected output ({e})"))
}

/// Datasets sorted by name, with snapshot numbers from the snapshot list.
pub fn parse(list_json: &[u8], snapshot_json: &[u8]) -> Result<Vec<Dataset>, String> {
    let list = parse_json(list_json, "zfs list")?;
    let snaps = parse_json(snapshot_json, "zfs list -t snapshot")?;
    let mut per: HashMap<&str, (u64, Option<i64>)> = HashMap::new();
    if let Some(m) = snaps.get("datasets").and_then(Value::as_object) {
        for (name, s) in m {
            let ds = s
                .get("dataset")
                .and_then(Value::as_str)
                .unwrap_or_else(|| name.split('@').next().unwrap_or(name));
            let created = num(prop(s, "creation")).map(|n| n as i64);
            let e = per.entry(ds).or_default();
            e.0 += 1;
            e.1 = e.1.max(created);
        }
    }
    let mut out = Vec::new();
    if let Some(m) = list.get("datasets").and_then(Value::as_object) {
        for (name, d) in m {
            let mountpoint = prop(d, "mountpoint")
                .and_then(Value::as_str)
                .filter(|m| m.starts_with('/'))
                .map(String::from);
            let kind = prop(d, "type")
                .and_then(Value::as_str)
                .or_else(|| d.get("type").and_then(Value::as_str))
                .unwrap_or("filesystem")
                .to_lowercase();
            let (snapshots, newest) = per.get(name.as_str()).copied().unwrap_or_default();
            out.push(Dataset {
                name: name.clone(),
                pool: d
                    .get("pool")
                    .and_then(Value::as_str)
                    .map(String::from)
                    .unwrap_or_else(|| name.split('/').next().unwrap_or(name).to_string()),
                kind,
                used: num(prop(d, "used")).unwrap_or(0),
                avail: num(prop(d, "available")).unwrap_or(0),
                quota: num(prop(d, "quota")).unwrap_or(0),
                mountpoint,
                snap_used: num(prop(d, "usedbysnapshots")).unwrap_or(0),
                snapshots,
                newest_snapshot: newest,
            });
        }
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
    fn datasets_with_snapshots_and_quota() {
        let ds = parse(&fixture("zfs-list.json"), &fixture("zfs-snapshots.json")).unwrap();
        assert_eq!(ds.len(), 15);
        assert_eq!(ds[0].name, "tank");
        assert_eq!(ds[0].mountpoint, None);
        assert_eq!(ds[0].snapshots, 1);
        let docs = ds.iter().find(|d| d.name == "tank/docs").unwrap();
        assert_eq!(docs.mountpoint.as_deref(), Some("/srv/docs"));
        assert_eq!(docs.kind, "filesystem");
        assert_eq!(docs.pool, "tank");
        assert_eq!(docs.snapshots, 4);
        assert_eq!(docs.newest_snapshot, Some(1_793_955_600));
        let t = ds.iter().find(|d| d.name == "tank/transfer").unwrap();
        assert_eq!(t.quota_percent(), Some(80));
        assert_eq!(t.snapshots, 0);
        assert_eq!(t.newest_snapshot, None);
        assert!(ds.iter().all(|d| d.avail > 0));
    }

    #[test]
    fn empty_outputs() {
        assert!(parse(b"", b"").unwrap().is_empty());
        assert!(parse(b"no datasets available\n", b"").unwrap().is_empty());
        assert!(parse(b"oops", b"").is_err());
    }
}
