//! Storage and host health for the admin dashboard and the alerts.
//!
//! The server takes a [`Snapshot`] every minute: ZFS pools and datasets (`zpool`/`zfs -j`,
//! readable by the service user), md arrays (`/proc/mdstat`), disks (`/sys/block`), the
//! host (/proc, /sys) and SMART data, which only root can read: `chestcraft collect` runs
//! from a timer as root and leaves it in `paths.smart_file`. Whatever the machine lacks is
//! [`Probe::Absent`] and left off the dashboard; nothing about one lab is built in.

pub mod alerts;
pub mod cmd;
pub mod collect;
pub mod disks;
pub mod history;
pub mod host;
pub mod mdstat;
pub mod smart;
pub mod zfs;
pub mod zpool;

use crate::config::Config;
use crate::db::Db;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::Duration;

/// How long a `zpool` or `zfs` call may take.
const TOOL_TIMEOUT: Duration = Duration::from_secs(20);
/// Largest SMART file the server reads.
const MAX_SMART_FILE: u64 = 16 << 20;
/// An upload counts as running while it got a chunk within this many seconds.
const TRANSFER_ACTIVE_SECS: i64 = 600;

/// Data from one source: not on this machine, read, or failed to read.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "state", content = "data", rename_all = "lowercase")]
pub enum Probe<T> {
    Absent,
    Ok(T),
    Failed(String),
}

impl<T> Probe<T> {
    pub fn ok(&self) -> Option<&T> {
        match self {
            Probe::Ok(v) => Some(v),
            _ => None,
        }
    }

    pub fn failed(&self) -> Option<&str> {
        match self {
            Probe::Failed(e) => Some(e),
            _ => None,
        }
    }
}

/// State of the collector's SMART file.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum SmartFile {
    Missing,
    Ok { collected: i64 },
    Failed { message: String },
}

/// A dataset with its place in chestcraft: volumes on it and the trash it holds.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DatasetInfo {
    #[serde(flatten)]
    pub ds: zfs::Dataset,
    pub volumes: Vec<String>,
    /// Bytes in chestcraft trash folders on this dataset.
    pub trash: u64,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct App {
    pub sessions: usize,
    pub transfers: usize,
    pub shares_active: usize,
    pub failed_logins_24h: usize,
    /// MCP tool calls by agents in the last 24 hours.
    pub agent_calls_24h: usize,
    /// Agent accounts, by name.
    pub agents: Vec<AgentActivity>,
    /// (account, role) for accounts whose role isn't in the config.
    pub missing_roles: Vec<(String, String)>,
    pub missing_shares: Vec<MissingShare>,
    /// The nightly state backup; None until the first one ran.
    pub backup: Option<crate::db::BackupStatus>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AgentActivity {
    pub name: String,
    pub role: String,
    pub disabled: bool,
    /// Newest use of any of its tokens (to the minute).
    pub last_call: Option<i64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MissingShare {
    pub id: String,
    pub owner: String,
    pub volume: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Snapshot {
    pub taken: i64,
    pub pools: Probe<Vec<zpool::Pool>>,
    pub datasets: Probe<Vec<DatasetInfo>>,
    pub md: Probe<Vec<mdstat::Array>>,
    pub disks: Vec<disks::Disk>,
    pub smart: SmartFile,
    pub host: host::Host,
    pub app: App,
}

impl Snapshot {
    /// A snapshot with nothing in it (tests, and before the first reading).
    pub fn empty(taken: i64) -> Snapshot {
        Snapshot {
            taken,
            pools: Probe::Absent,
            datasets: Probe::Absent,
            md: Probe::Absent,
            disks: Vec::new(),
            smart: SmartFile::Missing,
            host: host::Host::default(),
            app: App::default(),
        }
    }

    pub fn disk(&self, name: &str) -> Option<&disks::Disk> {
        self.disks.iter().find(|d| d.name == name)
    }
}

/// Where the machine's files are; tests point these at temp dirs.
#[derive(Debug, Clone)]
pub struct Roots {
    pub proc: std::path::PathBuf,
    pub sys: std::path::PathBuf,
    pub dev: std::path::PathBuf,
}

impl Default for Roots {
    fn default() -> Self {
        Roots {
            proc: "/proc".into(),
            sys: "/sys".into(),
            dev: "/dev".into(),
        }
    }
}

/// Is ZFS on this machine (tools installed and the module loaded)?
fn zfs_present(roots: &Roots) -> bool {
    Path::new(crate::tools::ZPOOL).exists() && roots.sys.join("module/zfs").exists()
}

async fn pools() -> Probe<Vec<zpool::Pool>> {
    let status = cmd::run(
        crate::tools::ZPOOL,
        &["status", "-j", "--json-int"],
        TOOL_TIMEOUT,
    );
    let list = cmd::run(
        crate::tools::ZPOOL,
        &[
            "list",
            "-j",
            "--json-int",
            "-o",
            "name,size,allocated,free,fragmentation,capacity,health",
        ],
        TOOL_TIMEOUT,
    );
    let (status, list) = tokio::join!(status, list);
    let run = || -> Result<Vec<zpool::Pool>, String> {
        let s = status?.into_stdout("zpool status")?;
        let l = list?.into_stdout("zpool list")?;
        zpool::parse(&s, &l)
    };
    match run() {
        Ok(p) => Probe::Ok(p),
        Err(e) => Probe::Failed(e),
    }
}

async fn datasets() -> Probe<Vec<zfs::Dataset>> {
    let list = cmd::run(crate::tools::ZFS, zfs::LIST_ARGS, TOOL_TIMEOUT);
    let snaps = cmd::run(crate::tools::ZFS, zfs::SNAPSHOT_ARGS, TOOL_TIMEOUT);
    let (list, snaps) = tokio::join!(list, snaps);
    let run = || -> Result<Vec<zfs::Dataset>, String> {
        let l = list?.into_stdout("zfs list")?;
        let s = snaps?.into_stdout("zfs list -t snapshot")?;
        zfs::parse(&l, &s)
    };
    match run() {
        Ok(d) => Probe::Ok(d),
        Err(e) => Probe::Failed(e),
    }
}

/// Read the collector's file. A file that isn't a regular file owned by root is ignored:
/// only the root collector may speak for the disks.
pub fn read_smart_file(
    path: &Path,
    require_root: bool,
) -> Result<Option<smart::CollectFile>, String> {
    use std::io::Read;
    use std::os::unix::fs::MetadataExt;
    let f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let meta = f.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() || (require_root && meta.uid() != 0) {
        return Err(format!("{}: not a file written by root", path.display()));
    }
    let mut bytes = Vec::new();
    f.take(MAX_SMART_FILE)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    let file: smart::CollectFile =
        serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    if file.version != smart::FILE_VERSION {
        return Err(format!(
            "{}: format {} (this chestcraft reads {})",
            path.display(),
            file.version,
            smart::FILE_VERSION
        ));
    }
    Ok(Some(file))
}

/// Disks with bay labels, pool and md membership (vdevs and members get their disk too).
pub fn assemble_disks(
    found: Vec<disks::BlockDisk>,
    labels: &BTreeMap<String, String>,
    pools: &mut Probe<Vec<zpool::Pool>>,
    md: &mut Probe<Vec<mdstat::Array>>,
    resolve_path: impl Fn(&str) -> Option<String>,
    parent: impl Fn(&str) -> Option<String>,
) -> Vec<disks::Disk> {
    let mut roles: HashMap<String, Vec<disks::Role>> = HashMap::new();
    if let Probe::Ok(pools) = pools {
        for p in pools.iter_mut() {
            fn fill(
                v: &mut zpool::Vdev,
                f: &dyn Fn(&str) -> Option<String>,
                out: &mut Vec<String>,
            ) {
                if v.is_leaf()
                    && let Some(path) = &v.path
                {
                    v.disk = f(path);
                    out.extend(v.disk.clone());
                }
                for c in &mut v.children {
                    fill(c, f, out);
                }
            }
            let mut members = Vec::new();
            for v in p.vdevs.iter_mut().chain(&mut p.spares).chain(&mut p.cache) {
                fill(v, &resolve_path, &mut members);
            }
            for d in members {
                roles
                    .entry(d)
                    .or_default()
                    .push(disks::Role::Pool(p.name.clone()));
            }
        }
    }
    if let Probe::Ok(arrays) = md {
        for a in arrays.iter_mut() {
            for m in &mut a.members {
                m.disk = parent(&m.dev);
                if let Some(d) = &m.disk {
                    roles
                        .entry(d.clone())
                        .or_default()
                        .push(disks::Role::Md(a.name.clone()));
                }
            }
        }
    }
    found
        .into_iter()
        .map(|b| {
            let mut r = roles.remove(&b.name).unwrap_or_default();
            r.sort();
            r.dedup();
            disks::Disk {
                label: b.serial.as_ref().and_then(|s| labels.get(s)).cloned(),
                roles: r,
                smart: None,
                smart_state: disks::SmartState::None,
                name: b.name,
                kind: b.kind,
                model: b.model,
                serial: b.serial,
                size: b.size,
            }
        })
        .collect()
}

/// Attach the collector's readings to the disks (by serial, else by device name).
pub fn attach_smart(
    disks: &mut [disks::Disk],
    file: &smart::CollectFile,
    labels: &BTreeMap<String, String>,
) {
    for d in disks.iter_mut() {
        let entry = file
            .disks
            .iter()
            .find(|c| c.serial.is_some() && c.serial == d.serial)
            .or_else(|| {
                file.disks
                    .iter()
                    .find(|c| c.device == d.name && (c.serial.is_none() || d.serial.is_none()))
            });
        let Some(c) = entry else {
            continue;
        };
        let parsed = c
            .smartctl
            .as_ref()
            .filter(|v| smart::has_data(v))
            .map(|v| smart::parse(v, c.checked));
        // Matched by device name, but the reading is of another disk (names moved).
        if let (Some(have), Some(read)) =
            (&d.serial, parsed.as_ref().and_then(|s| s.serial.as_ref()))
            && have != read
        {
            continue;
        }
        d.smart_state = match (&c.error, &parsed, &c.smartctl) {
            (Some(e), None, _) => disks::SmartState::Error { message: e.clone() },
            (_, None, Some(v)) if !smart::supported(v) => disks::SmartState::Unsupported,
            (_, None, _) => disks::SmartState::Error {
                message: c
                    .smartctl
                    .as_ref()
                    .and_then(smart::error_message)
                    .unwrap_or_else(|| "no SMART data".into()),
            },
            (_, Some(_), _) if c.standby => disks::SmartState::Standby,
            (_, Some(_), _) => disks::SmartState::Ok,
        };
        if let Some(s) = &parsed {
            if d.serial.is_none() {
                d.serial = s.serial.clone();
                d.label = d.serial.as_ref().and_then(|s| labels.get(s)).cloned();
            }
            if let Some(m) = &s.model {
                d.model = m.clone();
            }
        }
        d.smart = parsed;
    }
}

/// Which datasets hold which volumes, and how much trash sits on each.
pub fn dataset_info(cfg: &Config, db: &Db, datasets: Vec<zfs::Dataset>) -> Vec<DatasetInfo> {
    let by_mount = |p: &Path| -> Option<usize> {
        datasets
            .iter()
            .enumerate()
            .filter_map(|(i, d)| {
                let m = Path::new(d.mountpoint.as_deref()?);
                p.starts_with(m).then_some((i, m.as_os_str().len()))
            })
            .max_by_key(|(_, len)| *len)
            .map(|(i, _)| i)
    };
    let mut volumes: Vec<Vec<String>> = vec![Vec::new(); datasets.len()];
    for v in cfg.volumes.values() {
        if let Some(i) = by_mount(&v.path) {
            volumes[i].push(v.name.clone());
        }
    }
    let mut trash = vec![0u64; datasets.len()];
    for item in db.trash_items().unwrap_or_default() {
        let Some(v) = cfg.volumes.get(&item.volume) else {
            continue;
        };
        if let Some(i) = by_mount(&v.path.join(&item.fs_root)) {
            trash[i] += item.size;
        }
    }
    datasets
        .into_iter()
        .zip(volumes)
        .zip(trash)
        .map(|((ds, volumes), trash)| DatasetInfo { ds, volumes, trash })
        .collect()
}

/// Sessions, transfers, shares and failed sign-ins from the database.
pub fn app_stats(cfg: &Config, db: &Db, vols: &crate::fs::volume::Volumes, now: i64) -> App {
    let mut app = App::default();
    if let Ok(users) = db.users() {
        let tokens = db.agent_tokens().unwrap_or_default();
        for u in users {
            if u.is_agent() {
                let key = crate::db::User::key(&u.name);
                app.agents.push(AgentActivity {
                    name: u.name.clone(),
                    role: u.role.clone(),
                    disabled: u.disabled,
                    last_call: tokens
                        .iter()
                        .filter(|(_, t)| t.agent == key)
                        .filter_map(|(_, t)| t.last_used)
                        .max(),
                });
            }
            if u.role != crate::auth::perm::ADMIN_ROLE && !cfg.roles.contains_key(&u.role) {
                app.missing_roles.push((u.name, u.role));
            }
        }
        app.agents.sort_by_key(|a| a.name.to_lowercase());
    }
    app.backup = db.backup_status().ok().flatten();
    app.sessions = db.session_count().unwrap_or(0) as usize;
    app.transfers = db
        .uploads()
        .map(|u| {
            u.iter()
                .filter(|u| now - u.touched < TRANSFER_ACTIVE_SECS)
                .count()
        })
        .unwrap_or(0);
    if let Ok(evs) = db.audit_since(now - 86_400) {
        app.failed_logins_24h = evs
            .iter()
            .filter(|e| e.action == "login_failed" || e.action == "webdav_login_failed")
            .count();
        app.agent_calls_24h = evs
            .iter()
            .filter(|e| e.actor.starts_with("agent:") && e.action.starts_with("mcp_"))
            .count();
    }
    for share in db.shares().unwrap_or_default() {
        if share.revoked.is_some() || share.expires.is_some_and(|t| t <= now) {
            continue;
        }
        match crate::shares::status(cfg, vols, db, &share, now) {
            Ok((crate::shares::Status::Active, _)) => app.shares_active += 1,
            Ok((crate::shares::Status::Missing, owner)) => app.missing_shares.push(MissingShare {
                id: share.id.clone(),
                owner: owner.map(|u| u.name).unwrap_or(share.owner.clone()),
                volume: share.volume.clone(),
                path: share.path.clone(),
            }),
            _ => {}
        }
    }
    app
}

/// Everything the dashboard shows, read now.
pub async fn gather(
    cfg: &Config,
    db: std::sync::Arc<Db>,
    vols: &crate::fs::volume::Volumes,
    roots: &Roots,
    arc_prev: Option<(u64, u64)>,
) -> Snapshot {
    let now = jiff::Timestamp::now().as_second();
    let zfs = zfs_present(roots);
    let (mut pools, datasets) = if zfs {
        tokio::join!(pools(), datasets())
    } else {
        (Probe::Absent, Probe::Absent)
    };
    let cfg2 = cfg.clone();
    let roots2 = roots.clone();
    let db2 = db.clone();
    let vols2 = vols.clone();
    let blocking = tokio::task::spawn_blocking(move || {
        let roots = roots2;
        let mut md = match std::fs::read_to_string(roots.proc.join("mdstat")) {
            Ok(text) => {
                let arrays = mdstat::parse(&text);
                if arrays.is_empty() {
                    Probe::Absent
                } else {
                    Probe::Ok(arrays)
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Probe::Absent,
            Err(e) => Probe::Failed(format!("/proc/mdstat: {e}")),
        };
        let found = disks::discover(&roots.sys, &roots.dev);
        let mut disks = assemble_disks(
            found,
            &cfg2.disks,
            &mut pools,
            &mut md,
            |p| disks::disk_of_path(&roots.sys, p),
            |b| disks::parent_disk(&roots.sys, b),
        );
        let smart = match read_smart_file(&cfg2.paths.smart_file, true) {
            Ok(Some(file)) => {
                attach_smart(&mut disks, &file, &cfg2.disks);
                SmartFile::Ok {
                    collected: file.collected,
                }
            }
            Ok(None) => SmartFile::Missing,
            Err(message) => SmartFile::Failed { message },
        };
        let datasets = match datasets {
            Probe::Ok(d) => Probe::Ok(dataset_info(&cfg2, &db2, d)),
            Probe::Absent => Probe::Absent,
            Probe::Failed(e) => Probe::Failed(e),
        };
        let mut host = host::read(&roots.proc, &roots.sys, arc_prev);
        let names: Vec<&str> = disks.iter().map(|d| d.name.as_str()).collect();
        host.counters.disk = host::disk_bytes(&roots.proc, &names);
        Snapshot {
            taken: now,
            pools,
            datasets,
            md,
            disks,
            smart,
            host,
            app: app_stats(&cfg2, &db2, &vols2, now),
        }
    })
    .await;
    blocking.unwrap_or_else(|_| {
        tracing::error!("reading the machine's health crashed");
        Snapshot::empty(now)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(name: &str, serial: &str, kind: disks::Kind) -> disks::BlockDisk {
        disks::BlockDisk {
            name: name.into(),
            kind,
            model: "M".into(),
            serial: Some(serial.into()),
            size: 1,
        }
    }

    #[test]
    fn membership_labels_and_smart() {
        let fixture = |n: &str| {
            std::fs::read(format!("{}/fixtures/{n}", env!("CARGO_MANIFEST_DIR"))).unwrap()
        };
        let mut pools = Probe::Ok(
            zpool::parse(
                &fixture("zpool-status-raidz1.json"),
                &fixture("zpool-list.json"),
            )
            .unwrap(),
        );
        let mut md = Probe::Ok(mdstat::parse(
            "md0 : active raid1 nvme1n1p2[1] nvme0n1p2[0]\n      1 blocks [2/2] [UU]\n",
        ));
        let leaf_paths: Vec<String> = pools.ok().unwrap()[0].vdevs[0]
            .children
            .iter()
            .map(|v| v.path.clone().unwrap())
            .collect();
        let found = vec![
            block("sda", "WD-WMEXAMPLE0001", disks::Kind::Hdd),
            block("sdb", "S2", disks::Kind::Hdd),
            block("sdc", "S3", disks::Kind::Hdd),
            block("sdd", "S4", disks::Kind::Hdd),
            block("nvme0n1", "NVEXAMPLE0003", disks::Kind::Nvme),
            block("nvme1n1", "N2", disks::Kind::Nvme),
            block("sde", "USB1", disks::Kind::Hdd),
        ];
        let labels = BTreeMap::from([("S2".to_string(), "Bay 2".to_string())]);
        let names = ["sda", "sdb", "sdc", "sdd"];
        let mut disks = assemble_disks(
            found,
            &labels,
            &mut pools,
            &mut md,
            |p| {
                leaf_paths
                    .iter()
                    .position(|l| l == p)
                    .map(|i| names[i].to_string())
            },
            |b| Some(b.trim_end_matches("p2").to_string()),
        );
        assert_eq!(disks[0].roles, [disks::Role::Pool("tank".into())]);
        assert_eq!(disks[1].label.as_deref(), Some("Bay 2"));
        assert_eq!(disks[4].roles, [disks::Role::Md("md0".into())]);
        assert!(disks[6].roles.is_empty());
        assert_eq!(
            pools.ok().unwrap()[0].vdevs[0].children[3].disk.as_deref(),
            Some("sdd")
        );
        assert_eq!(
            md.ok().unwrap()[0].members[0].disk.as_deref(),
            Some("nvme1n1")
        );

        let json = |n: &str| serde_json::from_slice(&fixture(n)).unwrap();
        let file = smart::CollectFile {
            version: smart::FILE_VERSION,
            collected: 5000,
            disks: vec![
                smart::CollectedDisk {
                    device: "sdz".into(),
                    serial: Some("WD-WMEXAMPLE0001".into()),
                    checked: 4000,
                    standby: true,
                    error: None,
                    smartctl: Some(json("smartctl-ata.json")),
                },
                smart::CollectedDisk {
                    device: "nvme0n1".into(),
                    serial: None,
                    checked: 5000,
                    standby: false,
                    error: None,
                    smartctl: Some(json("smartctl-nvme.json")),
                },
                smart::CollectedDisk {
                    device: "sdb".into(),
                    serial: Some("S2".into()),
                    checked: 5000,
                    standby: false,
                    error: Some("smartctl didn't answer".into()),
                    smartctl: None,
                },
                // Named like sdc, but the reading belongs to another disk.
                smart::CollectedDisk {
                    device: "sdc".into(),
                    serial: None,
                    checked: 5000,
                    standby: false,
                    error: None,
                    smartctl: Some(json("smartctl-ata-failing.json")),
                },
            ],
        };
        attach_smart(&mut disks, &file, &labels);
        // Matched by serial although the device name changed; asleep.
        assert_eq!(disks[0].smart_state, disks::SmartState::Standby);
        assert_eq!(disks[0].smart.as_ref().unwrap().checked, 4000);
        assert_eq!(disks[0].model, "WDC WD30EFRX-68AX9N0");
        assert!(matches!(
            disks[1].smart_state,
            disks::SmartState::Error { .. }
        ));
        // Matched by device name; the reading's serial agrees.
        assert_eq!(disks[4].smart_state, disks::SmartState::Ok);
        assert_eq!(disks[2].smart_state, disks::SmartState::None);
        assert_eq!(disks[6].smart_state, disks::SmartState::None);
    }

    #[tokio::test]
    async fn gather_reads_this_machine() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::from_toml_str(&format!(
            "[server]\npublic_url = \"https://files.example.org\"\n[paths]\nsmart_file = \"{}/smart.json\"\n",
            dir.path().display()
        ))
        .unwrap();
        let db = std::sync::Arc::new(Db::open(&dir.path().join("t.redb")).unwrap());
        let vols = crate::fs::volume::Volumes::open(&cfg);
        let s = gather(&cfg, db, &vols, &Roots::default(), None).await;
        assert!(s.host.uptime_secs.is_some() && s.host.root_fs.is_some());
        assert_eq!(s.smart, SmartFile::Missing);
        assert!(s.disks.iter().all(|d| d.smart.is_none()));
    }

    #[test]
    fn smart_file_must_belong_to_root() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("smart.json");
        assert_eq!(read_smart_file(&p, true).unwrap(), None);
        let file = smart::CollectFile {
            version: smart::FILE_VERSION,
            collected: 1,
            disks: vec![],
        };
        std::fs::write(&p, serde_json::to_vec(&file).unwrap()).unwrap();
        assert!(read_smart_file(&p, true).unwrap_err().contains("root"));
        assert_eq!(read_smart_file(&p, false).unwrap(), Some(file));
        std::fs::write(&p, br#"{"version":99,"collected":1,"disks":[]}"#).unwrap();
        assert!(
            read_smart_file(&p, false)
                .unwrap_err()
                .contains("format 99")
        );
    }
}
