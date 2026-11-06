//! `chestcraft collect`: run as root from `chestcraft-collect.timer`, read SMART data of
//! every disk with `smartctl -j -a -n standby` (sleeping disks are not woken; their last
//! reading is kept) and write it to `paths.smart_file` for the server. It never reads
//! anything the service user can write: the output folder must belong to root and not be
//! writable by others.

use super::smart::{self, CollectFile, CollectedDisk};
use super::{Roots, cmd, disks};
use crate::config::Config;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::time::Duration;

const SMARTCTL_TIMEOUT: Duration = Duration::from_secs(90);

/// smartctl exit status bits 0 and 1: bad command line, or the device couldn't be opened
/// (also: asleep with `-n standby`). Higher bits report findings, not failures.
const NO_DATA_BITS: i32 = 0b11;

/// Check that `dir` may hold the file: owned by root, not writable by group or others.
pub fn check_dir(dir: &Path) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if !meta.is_dir() {
        return Err(format!("{} is not a folder", dir.display()));
    }
    if meta.uid() != 0 || meta.mode() & 0o022 != 0 {
        return Err(format!(
            "{} must belong to root and be writable only by root",
            dir.display()
        ));
    }
    Ok(())
}

/// One disk's reading; `prev` is its entry from the last run.
pub fn reading(
    device: &str,
    serial: Option<String>,
    out: Result<cmd::Output, String>,
    prev: Option<&CollectedDisk>,
    now: i64,
) -> CollectedDisk {
    let mut entry = CollectedDisk {
        device: device.to_string(),
        serial,
        checked: now,
        standby: false,
        error: None,
        smartctl: None,
    };
    let out = match out {
        Ok(o) => o,
        Err(e) => {
            entry.error = Some(e);
            return keep_previous(entry, prev);
        }
    };
    let json: Option<serde_json::Value> = serde_json::from_slice(&out.stdout).ok();
    let Some(json) = json else {
        entry.error = Some(if out.stderr.is_empty() {
            "smartctl printed no JSON".into()
        } else {
            out.stderr
        });
        return keep_previous(entry, prev);
    };
    let code = out.code.unwrap_or(-1);
    if smart::in_standby(&json) {
        entry.standby = true;
        return keep_previous(entry, prev);
    }
    if code < 0 || (code & NO_DATA_BITS != 0 && !smart::has_data(&json)) {
        entry.error = Some(
            smart::error_message(&json).unwrap_or_else(|| format!("smartctl exit status {code}")),
        );
        entry.smartctl = Some(json);
        return keep_previous(entry, prev);
    }
    if entry.serial.is_none() {
        entry.serial = json
            .get("serial_number")
            .and_then(|s| s.as_str())
            .map(|s| s.trim().to_string());
    }
    entry.smartctl = Some(json);
    entry
}

/// A disk without a fresh reading keeps its last good one (with that one's time).
fn keep_previous(mut entry: CollectedDisk, prev: Option<&CollectedDisk>) -> CollectedDisk {
    if let Some(p) = prev.filter(|p| p.smartctl.as_ref().is_some_and(smart::has_data)) {
        entry.smartctl = p.smartctl.clone();
        entry.checked = p.checked;
        if entry.serial.is_none() {
            entry.serial = p.serial.clone();
        }
    }
    entry
}

/// Write `file` to `path` atomically (temp file, fsync, rename), mode 0644.
pub fn write_file(path: &Path, file: &CollectFile) -> Result<(), String> {
    let dir = path
        .parent()
        .ok_or_else(|| format!("{} has no folder", path.display()))?;
    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("smart"),
        std::process::id()
    ));
    let bytes = serde_json::to_vec(file).map_err(|e| e.to_string())?;
    let result = (|| -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(&tmp)?;
        f.set_permissions(std::fs::Permissions::from_mode(0o644))?;
        f.write_all(&bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("{}: {e}", path.display()));
    }
    Ok(())
}

/// The whole run. Returns a summary line for the journal.
pub fn run(cfg: &Config) -> Result<String, String> {
    let path = &cfg.paths.smart_file;
    let dir = path
        .parent()
        .ok_or_else(|| format!("paths.smart_file: {} has no folder", path.display()))?;
    if !dir.exists() {
        std::fs::create_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    check_dir(dir)?;
    let prev = super::read_smart_file(path, true).ok().flatten();
    let roots = Roots::default();
    let found = disks::discover(&roots.sys, &roots.dev);
    let now = jiff::Timestamp::now().as_second();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let mut entries = Vec::new();
    for d in &found {
        let dev = format!("/dev/{}", d.name);
        let out = rt.block_on(cmd::run(
            crate::tools::SMARTCTL,
            &["-j", "-a", "-n", "standby", &dev],
            SMARTCTL_TIMEOUT,
        ));
        let previous = prev.as_ref().and_then(|p| {
            p.disks
                .iter()
                .find(|c| c.serial.is_some() && c.serial == d.serial)
                .or_else(|| p.disks.iter().find(|c| c.device == d.name))
        });
        entries.push(reading(&d.name, d.serial.clone(), out, previous, now));
    }
    let file = CollectFile {
        version: smart::FILE_VERSION,
        collected: now,
        disks: entries,
    };
    write_file(path, &file)?;
    let asleep = file.disks.iter().filter(|d| d.standby).count();
    let failed = file.disks.iter().filter(|d| d.error.is_some()).count();
    Ok(format!(
        "{} disks read ({asleep} asleep, {failed} failed) → {}",
        file.disks.len(),
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(code: i32, fixture: &str) -> Result<cmd::Output, String> {
        Ok(cmd::Output {
            code: Some(code),
            stdout: std::fs::read(format!("{}/fixtures/{fixture}", env!("CARGO_MANIFEST_DIR")))
                .unwrap(),
            stderr: String::new(),
        })
    }

    #[test]
    fn readings_keep_the_last_good_data_while_asleep_or_failing() {
        let good = reading("sda", None, out(0, "smartctl-ata.json"), None, 100);
        assert_eq!(good.serial.as_deref(), Some("WD-WMEXAMPLE0001"));
        assert!(good.error.is_none() && !good.standby && good.smartctl.is_some());
        // A failing disk sets high exit bits but still has data.
        let failing = reading(
            "sdc",
            None,
            out(0xD8, "smartctl-ata-failing.json"),
            None,
            100,
        );
        assert!(failing.error.is_none() && failing.smartctl.is_some());

        let asleep = reading(
            "sda",
            None,
            out(2, "smartctl-standby.json"),
            Some(&good),
            200,
        );
        assert!(asleep.standby);
        assert_eq!(asleep.checked, 100);
        assert_eq!(asleep.smartctl, good.smartctl);
        assert_eq!(asleep.serial.as_deref(), Some("WD-WMEXAMPLE0001"));

        let hung = reading(
            "sda",
            None,
            Err("smartctl didn't answer".into()),
            Some(&good),
            300,
        );
        assert_eq!(hung.error.as_deref(), Some("smartctl didn't answer"));
        assert_eq!(hung.checked, 100);

        let gone = reading(
            "sdx",
            None,
            Ok(cmd::Output {
                code: Some(2),
                stdout: br#"{"smartctl":{"exit_status":2,"messages":[{"string":"open failed","severity":"error"}]}}"#.to_vec(),
                stderr: String::new(),
            }),
            None,
            300,
        );
        assert_eq!(gone.error.as_deref(), Some("open failed"));
        assert!(!gone.smartctl.as_ref().is_some_and(smart::has_data));
    }

    #[test]
    fn atomic_write_and_folder_checks() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("smart.json");
        let file = CollectFile {
            version: smart::FILE_VERSION,
            collected: 1,
            disks: vec![],
        };
        write_file(&p, &file).unwrap();
        write_file(&p, &file).unwrap();
        assert_eq!(
            super::super::read_smart_file(&p, false).unwrap(),
            Some(file)
        );
        let mode = std::fs::metadata(&p).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o644);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        // A temp dir belongs to the test user, not root.
        assert!(check_dir(dir.path()).unwrap_err().contains("root"));
        assert!(check_dir(&p).is_err());
    }
}
