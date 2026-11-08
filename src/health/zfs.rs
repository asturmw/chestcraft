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
    "name,used,available,quota,refquota,referenced,mountpoint,usedbysnapshots,type",
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
    /// 0 = none. Limits `used` (the dataset's data, its snapshots and its children).
    pub quota: u64,
    /// 0 = none. Limits `referenced`.
    pub refquota: u64,
    /// Space the dataset's own data takes, without snapshots and children.
    pub referenced: u64,
    /// None for `none`, `legacy` and volumes.
    pub mountpoint: Option<String>,
    /// Space only snapshots hold.
    pub snap_used: u64,
    pub snapshots: u64,
    /// Creation time of the newest snapshot.
    pub newest_snapshot: Option<i64>,
}

impl Dataset {
    /// The limit closest to full, as (percent in use, limit): `quota` counts `used`,
    /// `refquota` counts `referenced`. None without either.
    pub fn quota_use(&self) -> Option<(u64, u64)> {
        let pct = |n: u64, limit: u64| (limit > 0).then(|| (n.saturating_mul(100) / limit, limit));
        match (
            pct(self.used, self.quota),
            pct(self.referenced, self.refquota),
        ) {
            (Some(q), Some(r)) => Some(if r.0 > q.0 { r } else { q }),
            (q, r) => q.or(r),
        }
    }

    /// Percent of the fuller quota in use.
    pub fn quota_percent(&self) -> Option<u64> {
        self.quota_use().map(|(pct, _)| pct)
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
                refquota: num(prop(d, "refquota")).unwrap_or(0),
                referenced: num(prop(d, "referenced")).unwrap_or(0),
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
        assert_eq!(t.quota_use(), Some((80, 600_000_000_000)));
        // refquota counts the dataset's own data (79.8 % of 3 TiB), not its snapshots.
        let video = ds.iter().find(|d| d.name == "tank/media/video").unwrap();
        assert_eq!(video.quota, 0);
        assert_eq!(video.referenced, 2_632_478_081_552);
        assert_eq!(video.quota_use(), Some((79, 3_298_534_883_328)));
        let backups = ds.iter().find(|d| d.name == "tank/backups").unwrap();
        assert_eq!(backups.quota_use().map(|(_, l)| l), Some(1_924_145_348_608));
        assert_eq!(ds[0].quota_use(), None);
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
