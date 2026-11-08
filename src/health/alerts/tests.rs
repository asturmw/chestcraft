use super::*;
use crate::config::Config;
use crate::health::disks::{Disk, Kind, SmartState};
use crate::health::{DatasetInfo, Probe, Snapshot, smart, zfs, zpool};

/// 2026-11-06 11:20 UTC: the fixture's last scrub ended 2026-10-11, the newest snapshot of
/// tank/docs is from 09:00.
const NOW: i64 = 1_793_964_000;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn cfg(extra: &str) -> Config {
    Config::from_toml_str(&format!(
        "[server]\npublic_url = \"https://files.example.org\"\n{extra}"
    ))
    .unwrap()
}

fn disk(name: &str, kind: Kind, smart_fixture: &str) -> Disk {
    let v: serde_json::Value = serde_json::from_slice(&fixture(smart_fixture)).unwrap();
    let s = smart::parse(&v, NOW - 600);
    Disk {
        name: name.into(),
        kind,
        model: s.model.clone().unwrap_or_default(),
        serial: s.serial.clone(),
        size: 1,
        label: None,
        roles: vec![],
        smart: Some(s),
        smart_state: SmartState::Ok,
    }
}

fn healthy() -> Snapshot {
    let mut s = Snapshot::empty(NOW);
    s.pools = Probe::Ok(
        zpool::parse(
            &fixture("zpool-status-raidz1.json"),
            &fixture("zpool-list.json"),
        )
        .unwrap(),
    );
    s.datasets = Probe::Ok(
        zfs::parse(&fixture("zfs-list.json"), &fixture("zfs-snapshots.json"))
            .unwrap()
            .into_iter()
            .map(|ds| DatasetInfo {
                ds,
                volumes: vec![],
                trash: 0,
            })
            .collect(),
    );
    s.disks = vec![
        disk("sda", Kind::Hdd, "smartctl-ata.json"),
        disk("nvme0n1", Kind::Nvme, "smartctl-nvme.json"),
    ];
    s.smart = SmartFile::Ok {
        collected: NOW - 600,
    };
    s.host.root_fs = Some(crate::health::host::FsUse {
        total: 100,
        used: 5,
        avail: 95,
    });
    s
}

/// Alert engine state across checks.
struct Engine {
    cfg: Config,
    active: BTreeMap<String, Active>,
    baselines: BTreeMap<String, Baseline>,
}

impl Engine {
    fn new(cfg: Config) -> Engine {
        Engine {
            cfg,
            active: BTreeMap::new(),
            baselines: BTreeMap::new(),
        }
    }

    fn check(&mut self, snap: &Snapshot, now: i64) -> Vec<Change> {
        let f = evaluate(
            snap,
            &self.cfg.alerts,
            &self.active,
            &mut self.baselines,
            now,
        );
        step(&mut self.active, f, now)
    }
}

fn kinds(changes: &[Change]) -> Vec<String> {
    changes
        .iter()
        .map(|c| {
            let e = c.log_entry(0);
            format!("{} {}", e.what, e.key)
        })
        .collect()
}

#[test]
fn healthy_machine_raises_nothing() {
    let mut e = Engine::new(cfg(""));
    assert_eq!(kinds(&e.check(&healthy(), NOW)), Vec::<String>::new());
    assert!(e.active.is_empty());
    // Counter baselines were taken quietly.
    assert!(e.baselines.contains_key("disk:WD-WMEXAMPLE0001:crc"));
    assert_eq!(verdict(e.active.values(), Some(0)), Verdict::Ok);
}

#[test]
fn degraded_pool_fires_once_repeats_criticals_daily_and_resolves() {
    // Keep the day-old SMART data from counting as stale.
    let mut e = Engine::new(cfg("[alerts.thresholds]\nstale_minutes = 10080\n"));
    let mut snap = healthy();
    let mut pools = zpool::parse(
        &fixture("zpool-status-degraded.json"),
        &fixture("zpool-list-two.json"),
    )
    .unwrap();
    pools[1].vdevs[0].children[1].disk = Some("sda".into());
    snap.pools = Probe::Ok(pools);
    let c = kinds(&e.check(&snap, NOW));
    assert_eq!(
        c,
        [
            "fired pool:fast:state",
            "fired pool:fast:data_errors",
            "fired pool:fast:full",
            "fired pool:fast:resilver",
        ]
    );
    let state = &e.active["pool:fast:state"];
    assert_eq!(state.severity, Severity::Critical);
    let i18n = I18n::builtin();
    let text = state.msg.render(&i18n, crate::i18n::EN);
    assert!(
        text.contains("fast") && text.contains("DEGRADED") && text.contains("WDC WD30EFRX-68AX9N0"),
        "{text}"
    );
    assert_eq!(e.active["pool:fast:full"].severity, Severity::Warning);
    assert_eq!(verdict(e.active.values(), Some(0)), Verdict::Critical);

    // Same state an hour later: quiet. A day later: the criticals again.
    assert!(e.check(&snap, NOW + 3600).is_empty());
    assert_eq!(
        kinds(&e.check(&snap, NOW + REPEAT_SECS)),
        ["repeated pool:fast:state", "repeated pool:fast:data_errors"]
    );

    // Resilver done, pool healthy again.
    let c = kinds(&e.check(&healthy(), NOW + REPEAT_SECS + 60));
    assert_eq!(c.len(), 4, "{c:?}");
    assert!(c.iter().all(|k| k.starts_with("resolved ")));
    assert!(e.active.is_empty());
    let ok = Msg::new("pool_resilver")
        .text("pool", "fast")
        .num("percent", 37);
    assert_eq!(
        ok.render_resolved(&i18n, crate::i18n::EN),
        "Pool fast finished resilvering"
    );
    let other = Msg::new("pool_data_errors")
        .text("pool", "fast")
        .num("count", 2);
    assert!(
        other
            .render_resolved(&i18n, crate::i18n::DE)
            .starts_with("Behoben: "),
        "{}",
        other.render_resolved(&i18n, crate::i18n::DE)
    );
}

fn with_capacity(cap: u64) -> Snapshot {
    let mut s = healthy();
    if let Probe::Ok(p) = &mut s.pools {
        p[0].capacity = cap;
    }
    s
}

#[test]
fn pool_capacity_with_hysteresis_and_quiet_downgrade() {
    let mut e = Engine::new(cfg(""));
    let sev = |e: &Engine| e.active.get("pool:tank:full").map(|a| a.severity);
    assert!(e.check(&with_capacity(80), NOW).is_empty());
    assert_eq!(
        kinds(&e.check(&with_capacity(81), NOW)),
        ["fired pool:tank:full"]
    );
    assert!(e.check(&with_capacity(79), NOW).is_empty());
    assert_eq!(sev(&e), Some(Severity::Warning));
    assert_eq!(
        kinds(&e.check(&with_capacity(91), NOW)),
        ["escalated pool:tank:full"]
    );
    // 89 % stays critical (hysteresis), 88 % drops to warning without a message.
    assert!(e.check(&with_capacity(89), NOW).is_empty());
    assert_eq!(sev(&e), Some(Severity::Critical));
    assert!(e.check(&with_capacity(88), NOW).is_empty());
    assert_eq!(sev(&e), Some(Severity::Warning));
    assert_eq!(
        kinds(&e.check(&with_capacity(78), NOW)),
        ["resolved pool:tank:full"]
    );
}

#[test]
fn missing_data_keeps_alerts_and_raises_its_own() {
    let mut e = Engine::new(cfg(""));
    let mut hot = healthy();
    hot.disks[0].smart.as_mut().unwrap().temp = Some(55);
    assert_eq!(
        kinds(&e.check(&hot, NOW)),
        ["fired disk:WD-WMEXAMPLE0001:temp"]
    );
    let mut full = with_capacity(85);
    full.disks = hot.disks.clone();
    e.check(&full, NOW);

    // zpool hangs, SMART data goes stale: nothing resolves, two new warnings.
    let mut broken = hot.clone();
    broken.pools = Probe::Failed("zpool didn't answer within 20 s".into());
    broken.datasets = Probe::Failed("zfs didn't answer within 20 s".into());
    broken.smart = SmartFile::Ok {
        collected: NOW - 3 * 3600,
    };
    let c = kinds(&e.check(&broken, NOW + 60));
    assert_eq!(
        c,
        ["fired probe:zpool", "fired probe:zfs", "fired smart:stale"]
    );
    assert!(e.active.contains_key("pool:tank:full"));
    assert!(e.active.contains_key("disk:WD-WMEXAMPLE0001:temp"));
    assert_eq!(verdict(e.active.values(), Some(0)), Verdict::Stale);
    let stale = e.active["smart:stale"]
        .msg
        .render(&I18n::builtin(), crate::i18n::EN);
    assert_eq!(stale, "Disk health data is 3 hours old");

    // Back to normal: everything resolves.
    let c = kinds(&e.check(&healthy(), NOW + 120));
    assert_eq!(c.len(), 5, "{c:?}");
    assert!(e.active.is_empty());

    // Hysteresis on temperatures: 3 °C.
    e.check(&hot, NOW);
    let mut cooler = healthy();
    cooler.disks[0].smart.as_mut().unwrap().temp = Some(48);
    assert!(e.check(&cooler, NOW).is_empty());
    cooler.disks[0].smart.as_mut().unwrap().temp = Some(47);
    assert_eq!(
        kinds(&e.check(&cooler, NOW)),
        ["resolved disk:WD-WMEXAMPLE0001:temp"]
    );
}

#[test]
fn counters_that_rise_are_events() {
    let mut e = Engine::new(cfg(""));
    e.check(&healthy(), NOW);
    let mut worse = healthy();
    worse.disks[0].smart.as_mut().unwrap().realloc = Some(8);
    worse.disks[1].smart.as_mut().unwrap().media_errors = Some(1);
    if let Probe::Ok(p) = &mut worse.pools {
        p[0].vdevs[0].children[2].cksum = 4;
    }
    let changes = e.check(&worse, NOW + 60);
    let c = kinds(&changes);
    assert_eq!(c.len(), 3, "{c:?}");
    assert!(c.iter().all(|k| k.starts_with("event ")));
    assert!(e.active.is_empty());
    let i18n = I18n::builtin();
    let texts: Vec<String> = changes
        .iter()
        .map(|c| match c {
            Change::Event(ev) => ev.msg.render(&i18n, crate::i18n::EN),
            _ => unreachable!(),
        })
        .collect();
    assert!(
        texts
            .iter()
            .any(|t| t.contains("reallocated") && t.contains("0 to 8")),
        "{texts:?}"
    );
    // Same values: quiet. A cleared counter lowers the baseline without an event.
    assert!(e.check(&worse, NOW + 120).is_empty());
    assert!(e.check(&healthy(), NOW + 180).is_empty());
    assert_eq!(e.baselines["disk:WD-WMEXAMPLE0001:realloc"].value, 0);
}

#[test]
fn failing_disk_md_rootfs_snapshots_and_app() {
    let mut e = Engine::new(cfg(
        "[alerts.snapshots]\n\"tank\" = \"2d\"\n\"tank/docs\" = \"2h\"\n\"tank/archive\" = \"0\"\n",
    ));
    let mut s = healthy();
    s.disks
        .push(disk("sdc", Kind::Hdd, "smartctl-ata-failing.json"));
    s.md = Probe::Ok(crate::health::mdstat::parse(
        "md0 : active raid1 nvme0n1p2[0]\n      1 blocks super 1.2 [2/1] [U_]\n",
    ));
    s.host.root_fs = Some(crate::health::host::FsUse {
        total: 100,
        used: 90,
        avail: 10,
    });
    if let Probe::Ok(ds) = &mut s.datasets {
        let t = ds
            .iter_mut()
            .find(|d| d.ds.name == "tank/transfer")
            .unwrap();
        t.ds.used = t.ds.quota / 100 * 95;
        // refquota counts the dataset's own data.
        let v = ds
            .iter_mut()
            .find(|d| d.ds.name == "tank/media/video")
            .unwrap();
        v.ds.referenced = v.ds.refquota / 100 * 96;
    }
    s.app.missing_roles = vec![("Kim".into(), "gast".into())];
    s.app.missing_shares = vec![crate::health::MissingShare {
        id: "AbCdEfGhJkLm".into(),
        owner: "jana".into(),
        volume: "Docs".into(),
        path: "Reports".into(),
    }];
    s.app.backup = Some(crate::db::BackupStatus {
        attempt: NOW,
        last_ok: None,
        error: Some("/srv/backup doesn't exist".into()),
        bytes: 0,
    });
    let c = kinds(&e.check(&s, NOW));
    for want in [
        "fired disk:WD-WMEXAMPLE0004:smart",
        "fired disk:WD-WMEXAMPLE0004:selftest",
        "fired md:md0:degraded",
        "fired host:rootfs",
        "fired user:kim:role",
        "fired share:AbCdEfGhJkLm:missing",
        "fired backup:failed",
        // Newest snapshot of tank/docs is 2h 20m old (limit 2h); tank/media/video's is 3
        // days old and tank's 30 (limit 2 days, from the rule for tank); datasets under
        // tank without any snapshot alert; tank/archive is switched off.
        "fired ds:tank/docs:snapshots",
        "fired ds:tank:snapshots",
        "fired ds:tank/media/video:snapshots",
        "fired ds:tank/media:snapshots",
        "fired ds:tank/transfer:quota",
        "fired ds:tank/media/video:quota",
    ] {
        assert!(c.contains(&want.to_string()), "{want} missing in {c:?}");
    }
    assert!(!c.iter().any(|k| k.contains("tank/archive")), "{c:?}");
    let i18n = I18n::builtin();
    let md = e.active["md:md0:degraded"]
        .msg
        .render(&i18n, crate::i18n::DE);
    assert!(md.contains("md0") && md.contains("U_"), "{md}");
}

#[test]
fn verdicts_and_cooldowns() {
    let a = |key: &str, severity| Active {
        key: key.into(),
        severity,
        msg: Msg::new("smart_missing"),
        since: 0,
        last_sent: 0,
    };
    assert_eq!(verdict([], Some(10)), Verdict::Ok);
    assert_eq!(verdict([], None), Verdict::Stale);
    assert_eq!(verdict([], Some(3600)), Verdict::Stale);
    let w = a("disk:x:temp", Severity::Warning);
    assert_eq!(verdict([&w], Some(10)), Verdict::Warning);
    let s = a("smart:stale", Severity::Warning);
    assert_eq!(verdict([&w, &s], Some(10)), Verdict::Stale);
    let c = a("pool:x:state", Severity::Critical);
    assert_eq!(verdict([&w, &s, &c], Some(10)), Verdict::Critical);

    let cd = Cooldowns::default();
    assert!(cd.allow("share_locked:x", 3600, 100));
    assert!(!cd.allow("share_locked:x", 3600, 200));
    assert!(cd.allow("share_locked:y", 3600, 200));
    assert!(cd.allow("share_locked:x", 3600, 3700));
}

#[test]
fn ages_read_naturally() {
    let i = I18n::builtin();
    assert_eq!(age(&i, crate::i18n::EN, 30), "1 minute");
    assert_eq!(age(&i, crate::i18n::EN, 45 * 60), "45 minutes");
    assert_eq!(age(&i, crate::i18n::EN, 3 * 3600 + 5), "3 hours");
    assert_eq!(age(&i, crate::i18n::DE, 5 * 86_400), "5 Tage");
}
