//! Physical disks from /sys/block (SATA, SAS, NVMe, virtio, …), and which pool or md array
//! each one belongs to. Works without privileges; SMART data comes from the collector.

use super::smart::Smart;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Block devices that are not disks.
const NOT_DISKS: &[&str] = &[
    "loop", "ram", "zram", "dm-", "md", "sr", "nbd", "fd", "zd", "drbd", "rbd",
];

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Hdd,
    Ssd,
    Nvme,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BlockDisk {
    pub name: String,
    pub kind: Kind,
    pub model: String,
    pub serial: Option<String>,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(tag = "type", content = "name", rename_all = "lowercase")]
pub enum Role {
    Pool(String),
    Md(String),
}

/// What the dashboard knows about SMART for one disk.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum SmartState {
    /// The collector hasn't reported this disk.
    None,
    Ok,
    /// Asleep at the last check; the data is from `checked`.
    Standby,
    /// smartctl couldn't read it.
    Error {
        message: String,
    },
    /// The device has no SMART (virtual disks, some USB bridges).
    Unsupported,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Disk {
    pub name: String,
    pub kind: Kind,
    pub model: String,
    pub serial: Option<String>,
    pub size: u64,
    /// Bay label from the config.
    pub label: Option<String>,
    pub roles: Vec<Role>,
    pub smart: Option<Smart>,
    pub smart_state: SmartState,
}

impl Disk {
    /// Stable id for alerts and history: the serial, else the device name.
    pub fn id(&self) -> String {
        match &self.serial {
            Some(s) => s.clone(),
            None => self.name.clone(),
        }
    }

    /// The serial's last 6 characters, for display.
    pub fn short_serial(&self) -> Option<String> {
        self.serial.as_ref().map(|s| {
            let chars: Vec<char> = s.chars().collect();
            if chars.len() <= 8 {
                s.clone()
            } else {
                format!("…{}", chars[chars.len() - 6..].iter().collect::<String>())
            }
        })
    }

    /// "Bay 2 (WDC WD40EFZX, …AMPLE2)" or "sdb (WDC WD40EFZX, …AMPLE2)".
    pub fn display_name(&self) -> String {
        let head = self.label.clone().unwrap_or_else(|| self.name.clone());
        let mut detail = Vec::new();
        if !self.model.is_empty() {
            detail.push(self.model.clone());
        }
        if let Some(s) = self.short_serial() {
            detail.push(s);
        }
        if detail.is_empty() {
            head
        } else {
            format!("{head} ({})", detail.join(", "))
        }
    }
}

fn read_trim(p: &Path) -> Option<String> {
    let s = std::fs::read_to_string(p).ok()?;
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// Unit serial number page (SCSI VPD 0x80): 4-byte header, then the serial.
pub fn parse_vpd80(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 4 || bytes[1] != 0x80 {
        return None;
    }
    let len = usize::from(bytes[3]).min(bytes.len() - 4);
    let s = String::from_utf8_lossy(&bytes[4..4 + len]);
    let s = s.trim_matches(|c: char| c.is_whitespace() || c == '\0');
    (!s.is_empty() && s.chars().all(|c| c.is_ascii_graphic())).then(|| s.to_string())
}

/// Serial from a `/dev/disk/by-id/ata-<model>_<serial>` link name.
fn serial_from_by_id(dev: &Path, name: &str) -> Option<String> {
    let dir = std::fs::read_dir(dev.join("disk/by-id")).ok()?;
    for e in dir.flatten() {
        let link = e.file_name().to_string_lossy().into_owned();
        let Some(rest) = link.strip_prefix("ata-") else {
            continue;
        };
        if link.contains("-part") {
            continue;
        }
        let target = std::fs::read_link(e.path()).ok()?;
        if target.file_name().is_some_and(|f| f == name) {
            return rest.rsplit_once('_').map(|(_, s)| s.to_string());
        }
    }
    None
}

/// Disks under `<sys>/block`, sorted by name. `dev` is where `disk/by-id` lives.
pub fn discover(sys: &Path, dev: &Path) -> Vec<BlockDisk> {
    let Ok(dir) = std::fs::read_dir(sys.join("block")) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in dir.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if NOT_DISKS.iter().any(|p| name.starts_with(p)) || name.contains("boot") {
            continue;
        }
        let base = e.path();
        let device = base.join("device");
        let real = std::fs::canonicalize(&base).unwrap_or_else(|_| base.clone());
        if !device.exists() || real.components().any(|c| c.as_os_str() == "virtual") {
            continue;
        }
        let sectors: u64 = read_trim(&base.join("size"))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if sectors == 0 {
            continue;
        }
        let nvme = name.starts_with("nvme");
        let rotational = read_trim(&base.join("queue/rotational")).as_deref() == Some("1");
        let serial = if nvme {
            read_trim(&device.join("serial"))
        } else {
            std::fs::read(device.join("vpd_pg80"))
                .ok()
                .and_then(|b| parse_vpd80(&b))
                .or_else(|| serial_from_by_id(dev, &name))
        };
        out.push(BlockDisk {
            kind: match (nvme, rotational) {
                (true, _) => Kind::Nvme,
                (false, true) => Kind::Hdd,
                (false, false) => Kind::Ssd,
            },
            model: read_trim(&device.join("model")).unwrap_or_default(),
            serial,
            size: sectors * 512,
            name,
        });
    }
    out.sort_by_key(|d| natural(&d.name));
    out
}

/// Sort key so sdb comes before sdaa and nvme2n1 before nvme10n1.
fn natural(name: &str) -> (usize, String) {
    (name.len(), name.to_string())
}

/// The disk a block device (`sdb1`, `nvme0n1p2`, `sdb`) lives on.
pub fn parent_disk(sys: &Path, block: &str) -> Option<String> {
    let link = sys.join("class/block").join(block);
    if !link.join("partition").exists() {
        return link.exists().then(|| block.to_string());
    }
    let real = std::fs::canonicalize(&link).ok()?;
    Some(real.parent()?.file_name()?.to_string_lossy().into_owned())
}

/// The disk behind a vdev path like `/dev/disk/by-partuuid/…` or `/dev/sdb1`.
pub fn disk_of_path(sys: &Path, path: &str) -> Option<String> {
    let real: PathBuf = std::fs::canonicalize(path).ok()?;
    let block = real.file_name()?.to_string_lossy().into_owned();
    parent_disk(sys, &block)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn vpd_serial() {
        let mut page = vec![0, 0x80, 0, 20];
        page.extend_from_slice(b"     WD-WXEXAMPLE001");
        assert_eq!(parse_vpd80(&page).as_deref(), Some("WD-WXEXAMPLE001"));
        assert_eq!(parse_vpd80(&[0, 0x83, 0, 0]), None);
        assert_eq!(parse_vpd80(&[0, 0x80, 0, 4, b' ', b' ', 0, 0]), None);
    }

    /// A small /sys + /dev: two SATA disks (one partitioned), an NVMe disk, md0 and loop0.
    fn fake_sys() -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        let r = t.path();
        let devices = r.join("sys/devices/pci0000:00");
        let mk = |rel: &str| {
            let p = devices.join(rel);
            std::fs::create_dir_all(&p).unwrap();
            p
        };
        let w = |p: &Path, s: &[u8]| std::fs::write(p, s).unwrap();
        std::fs::create_dir_all(r.join("sys/block")).unwrap();
        std::fs::create_dir_all(r.join("sys/class/block")).unwrap();
        for (i, disk, rot, model, serial) in [
            (0, "sda", "1", "WDC WD30EFRX-68A", "WD-WMEXAMPLE0001"),
            (1, "sdb", "0", "Samsung SSD 870", "S6EXAMPLE0002"),
        ] {
            let scsi = format!("ata{i}/host{i}/target{i}:0:0/{i}:0:0:0");
            let d = mk(&format!("{scsi}/block/{disk}"));
            let dev = devices.join(&scsi);
            w(&dev.join("model"), format!("{model}\n").as_bytes());
            let mut page = vec![0, 0x80, 0, serial.len() as u8];
            page.extend_from_slice(serial.as_bytes());
            w(&dev.join("vpd_pg80"), &page);
            symlink(&dev, d.join("device")).unwrap();
            std::fs::create_dir_all(d.join("queue")).unwrap();
            w(&d.join("queue/rotational"), format!("{rot}\n").as_bytes());
            w(&d.join("size"), b"5860533168\n");
            symlink(&d, r.join("sys/block").join(disk)).unwrap();
            symlink(&d, r.join("sys/class/block").join(disk)).unwrap();
        }
        let part = devices.join("ata0/host0/target0:0:0/0:0:0:0/block/sda/sda1");
        std::fs::create_dir_all(&part).unwrap();
        w(&part.join("partition"), b"1\n");
        symlink(&part, r.join("sys/class/block/sda1")).unwrap();

        let ctrl = mk("0000:07:00.0/nvme/nvme0");
        w(&ctrl.join("model"), b"EXAMPLE NVME 1TB  \n");
        w(&ctrl.join("serial"), b"NVEXAMPLE0003   \n");
        let ns = mk("0000:07:00.0/nvme/nvme0/nvme0n1");
        symlink(&ctrl, ns.join("device")).unwrap();
        std::fs::create_dir_all(ns.join("queue")).unwrap();
        w(&ns.join("queue/rotational"), b"0\n");
        w(&ns.join("size"), b"2000409264\n");
        symlink(&ns, r.join("sys/block/nvme0n1")).unwrap();
        symlink(&ns, r.join("sys/class/block/nvme0n1")).unwrap();
        let p2 = ns.join("nvme0n1p2");
        std::fs::create_dir_all(&p2).unwrap();
        w(&p2.join("partition"), b"2\n");
        symlink(&p2, r.join("sys/class/block/nvme0n1p2")).unwrap();

        let md = r.join("sys/devices/virtual/block/md0");
        std::fs::create_dir_all(md.join("md")).unwrap();
        w(&md.join("size"), b"100\n");
        symlink(&md, r.join("sys/block/md0")).unwrap();
        let lo = r.join("sys/devices/virtual/block/loop0");
        std::fs::create_dir_all(&lo).unwrap();
        symlink(&lo, r.join("sys/block/loop0")).unwrap();

        // /dev with a by-partuuid link to sda1.
        std::fs::create_dir_all(r.join("dev/disk/by-partuuid")).unwrap();
        w(&r.join("dev/sda1"), b"");
        symlink(r.join("dev/sda1"), r.join("dev/disk/by-partuuid/0000-1111")).unwrap();
        t
    }

    #[test]
    fn discovery_and_membership() {
        let t = fake_sys();
        let sys = t.path().join("sys");
        let disks = discover(&sys, &t.path().join("dev"));
        assert_eq!(
            disks
                .iter()
                .map(|d| (d.name.as_str(), d.kind, d.serial.as_deref()))
                .collect::<Vec<_>>(),
            [
                ("sda", Kind::Hdd, Some("WD-WMEXAMPLE0001")),
                ("sdb", Kind::Ssd, Some("S6EXAMPLE0002")),
                ("nvme0n1", Kind::Nvme, Some("NVEXAMPLE0003")),
            ]
        );
        assert_eq!(disks[2].model, "EXAMPLE NVME 1TB");
        assert_eq!(disks[0].size, 5_860_533_168 * 512);
        assert_eq!(parent_disk(&sys, "sda1").as_deref(), Some("sda"));
        assert_eq!(parent_disk(&sys, "nvme0n1p2").as_deref(), Some("nvme0n1"));
        assert_eq!(parent_disk(&sys, "sdb").as_deref(), Some("sdb"));
        assert_eq!(parent_disk(&sys, "sdz9"), None);
        let link = t.path().join("dev/disk/by-partuuid/0000-1111");
        assert_eq!(
            disk_of_path(&sys, link.to_str().unwrap()).as_deref(),
            Some("sda")
        );
    }

    #[test]
    fn names() {
        let d = Disk {
            name: "sdb".into(),
            kind: Kind::Hdd,
            model: "WDC WD40EFZX".into(),
            serial: Some("WD-WXEXAMPLE29".into()),
            size: 0,
            label: Some("Bay 2".into()),
            roles: vec![],
            smart: None,
            smart_state: SmartState::None,
        };
        assert_eq!(d.display_name(), "Bay 2 (WDC WD40EFZX, …MPLE29)");
        assert_eq!(d.id(), "WD-WXEXAMPLE29");
        let bare = Disk {
            label: None,
            model: String::new(),
            serial: None,
            ..d
        };
        assert_eq!(bare.display_name(), "sdb");
        assert_eq!(bare.id(), "sdb");
    }
}
