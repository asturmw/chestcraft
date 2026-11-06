//! Linux software RAID from `/proc/mdstat`.

use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Array {
    pub name: String,
    /// active, inactive (read-only arrays report "active (auto-read-only)" → active).
    pub state: String,
    /// raid1, raid5, raid0, …; empty for inactive arrays.
    pub level: String,
    pub members: Vec<Member>,
    /// `[total/up]`; None for levels without redundancy (raid0, linear).
    pub total: Option<u32>,
    pub up: Option<u32>,
    /// `[UU_]`
    pub status: String,
    /// A running resync, recovery, check or reshape.
    pub sync: Option<Sync>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Member {
    /// Block device (`sda1`, `nvme0n1p2`).
    pub dev: String,
    pub faulty: bool,
    pub spare: bool,
    /// Parent disk, filled in by [`crate::health::disks`].
    pub disk: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Sync {
    /// resync, recovery, check, repair or reshape.
    pub action: String,
    pub percent: f64,
    /// "9.4min" as printed.
    pub finish: Option<String>,
}

impl Array {
    pub fn inactive(&self) -> bool {
        self.state != "active"
    }

    /// Fewer members up than the array needs, or a faulty member.
    pub fn degraded(&self) -> bool {
        matches!((self.total, self.up), (Some(t), Some(u)) if u < t)
            || self.status.contains('_')
            || self.members.iter().any(|m| m.faulty)
    }
}

/// Parse `/proc/mdstat`; arrays in file order.
pub fn parse(text: &str) -> Vec<Array> {
    let mut out: Vec<Array> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !line.starts_with(char::is_whitespace) {
            // "md0 : active raid1 nvme1n1p2[1] nvme0n1p2[0]"
            let Some((name, rest)) = line.split_once(" : ") else {
                continue;
            };
            let name = name.trim();
            if !name.starts_with("md") {
                continue;
            }
            let mut words = rest.split_whitespace().peekable();
            let state = words.next().unwrap_or("").to_string();
            // "(auto-read-only)" / "(read-only)" follow the state.
            while words.peek().is_some_and(|w| w.starts_with('(')) {
                words.next();
            }
            let mut level = String::new();
            let mut members = Vec::new();
            for w in words {
                if let Some(bracket) = w.find('[') {
                    let dev = w[..bracket].to_string();
                    let flags = &w[bracket..];
                    members.push(Member {
                        dev,
                        faulty: flags.contains("(F)"),
                        spare: flags.contains("(S)"),
                        disk: None,
                    });
                } else if level.is_empty() && members.is_empty() {
                    level = w.to_string();
                }
            }
            out.push(Array {
                name: name.to_string(),
                state,
                level,
                members,
                total: None,
                up: None,
                status: String::new(),
                sync: None,
            });
            continue;
        }
        let Some(array) = out.last_mut() else {
            continue;
        };
        // "124468224 blocks super 1.2 [2/2] [UU]"
        if trimmed.contains(" blocks") {
            for w in trimmed.split_whitespace() {
                if let Some(inner) = w.strip_prefix('[').and_then(|w| w.strip_suffix(']')) {
                    if let Some((t, u)) = inner.split_once('/') {
                        array.total = t.parse().ok();
                        array.up = u.parse().ok();
                    } else if !inner.is_empty() && inner.chars().all(|c| c == 'U' || c == '_') {
                        array.status = inner.to_string();
                    }
                }
            }
            continue;
        }
        // "[=>....]  recovery =  8.5% (10518528/124468224) finish=9.4min speed=201345K/sec"
        if let Some(eq) = trimmed.find(" = ") {
            let action = trimmed[..eq]
                .split_whitespace()
                .last()
                .unwrap_or("")
                .to_string();
            let rest = &trimmed[eq + 3..];
            let percent = rest
                .split_whitespace()
                .next()
                .and_then(|p| p.strip_suffix('%'))
                .and_then(|p| p.parse().ok());
            if let Some(percent) = percent
                && !action.is_empty()
            {
                array.sync = Some(Sync {
                    action,
                    percent,
                    finish: rest
                        .split_whitespace()
                        .find_map(|w| w.strip_prefix("finish="))
                        .map(String::from),
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn healthy_mirror() {
        let a = parse(
            "Personalities : [raid1] [raid0] [raid6] [raid5] [raid4] [raid10] \n\
             md0 : active raid1 nvme1n1p2[1] nvme0n1p2[0]\n\
             \x20     124468224 blocks super 1.2 [2/2] [UU]\n\
             \x20     bitmap: 1/1 pages [4KB], 65536KB chunk\n\
             \n\
             unused devices: <none>\n",
        );
        assert_eq!(a.len(), 1);
        let md = &a[0];
        assert_eq!((md.name.as_str(), md.level.as_str()), ("md0", "raid1"));
        assert_eq!(
            md.members
                .iter()
                .map(|m| m.dev.as_str())
                .collect::<Vec<_>>(),
            ["nvme1n1p2", "nvme0n1p2"]
        );
        assert_eq!(
            (md.total, md.up, md.status.as_str()),
            (Some(2), Some(2), "UU")
        );
        assert!(!md.degraded() && !md.inactive() && md.sync.is_none());
    }

    #[test]
    fn degraded_recovering_raid0_and_inactive() {
        let a = parse(
            "Personalities : [raid1] [raid6] [raid5] [raid4]\n\
             md1 : active raid5 sdd1[4] sdc1[2] sdb1[1](F) sda1[0]\n\
             \x20     5860270080 blocks super 1.2 level 5, 512k chunk, algorithm 2 [3/2] [U_U]\n\
             \x20     [=>...................]  recovery =  8.5% (249256960/2930135040) finish=211.2min speed=211475K/sec\n\
             \n\
             md2 : active (auto-read-only) raid0 sde[0] sdf[1]\n\
             \x20     1953260544 blocks super 1.2 512k chunks\n\
             \n\
             md127 : inactive sdg[0](S)\n\
             \x20     976631512 blocks super 1.2\n\
             \n\
             unused devices: <none>\n",
        );
        assert_eq!(a.len(), 3);
        let md1 = &a[0];
        assert!(md1.degraded());
        assert!(md1.members.iter().any(|m| m.dev == "sdb1" && m.faulty));
        let s = md1.sync.as_ref().unwrap();
        assert_eq!(s.action, "recovery");
        assert!((s.percent - 8.5).abs() < 1e-9);
        assert_eq!(s.finish.as_deref(), Some("211.2min"));
        let md2 = &a[1];
        assert_eq!(
            (md2.state.as_str(), md2.level.as_str()),
            ("active", "raid0")
        );
        assert!(!md2.degraded() && md2.total.is_none());
        let md127 = &a[2];
        assert!(md127.inactive());
        assert!(md127.members[0].spare);
        assert!(parse("Personalities : \nunused devices: <none>\n").is_empty());
    }
}
