//! SMART data: what `smartctl -j -a` reports for ATA, SCSI/SAS and NVMe disks, boiled down
//! to what the dashboard and the alerts use, and the file the root collector writes.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const FILE_VERSION: u32 = 1;

/// Written by `chestcraft collect` (root), read by the server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CollectFile {
    pub version: u32,
    /// When the collector ran.
    pub collected: i64,
    pub disks: Vec<CollectedDisk>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CollectedDisk {
    /// Block device name (`sda`).
    pub device: String,
    #[serde(default)]
    pub serial: Option<String>,
    /// When `smartctl` is from (older than `collected` while the disk sleeps).
    pub checked: i64,
    /// The disk was in standby; `smartctl` is the last reading taken while it was awake.
    #[serde(default)]
    pub standby: bool,
    #[serde(default)]
    pub error: Option<String>,
    /// smartctl's JSON output.
    #[serde(default)]
    pub smartctl: Option<Value>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Ata,
    Scsi,
    Nvme,
    Other,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Smart {
    pub checked: i64,
    pub protocol: Protocol,
    pub model: Option<String>,
    pub serial: Option<String>,
    /// Overall health self-assessment.
    pub passed: Option<bool>,
    pub temp: Option<i32>,
    pub power_on_hours: Option<u64>,
    pub realloc: Option<u64>,
    pub pending: Option<u64>,
    pub uncorrectable: Option<u64>,
    pub crc: Option<u64>,
    /// Percent of rated endurance used (NVMe, some SATA SSDs).
    pub wear: Option<u64>,
    pub nvme_critical_warning: Option<u64>,
    pub media_errors: Option<u64>,
    pub available_spare: Option<u64>,
    pub self_test: Option<SelfTest>,
    pub self_test_running: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SelfTest {
    /// "short", "extended", "conveyance" or what smartctl calls it.
    pub kind: String,
    /// None while running or when aborted.
    pub passed: Option<bool>,
    /// smartctl's text, e.g. "Completed without error".
    pub result: String,
    /// Power-on hours when it ran.
    pub hours: Option<u64>,
}

impl Smart {
    /// Roughly when the newest self-test ran (assumes the disk was powered on since).
    pub fn self_test_time(&self) -> Option<i64> {
        let st = self.self_test.as_ref()?;
        let ago = self.power_on_hours?.checked_sub(st.hours?)?;
        Some(self.checked - ago as i64 * 3600)
    }
}

fn u(v: Option<&Value>) -> Option<u64> {
    match v? {
        Value::Number(n) => n.as_u64().or_else(|| n.as_i64().map(|i| i.max(0) as u64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn path<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().try_fold(v, |v, k| v.get(k))
}

fn s(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// Raw value of an ATA attribute by id (the low 48 bits hold the count).
fn ata_attr(v: &Value, id: u64) -> Option<u64> {
    path(v, &["ata_smart_attributes", "table"])?
        .as_array()?
        .iter()
        .find(|a| u(a.get("id")) == Some(id))
        .and_then(|a| u(path(a, &["raw", "value"])))
        .map(|raw| raw & 0xFFFF_FFFF_FFFF)
}

fn test_kind(raw: &str) -> String {
    let l = raw.to_lowercase();
    if l.starts_with("short") {
        "short".into()
    } else if l.starts_with("extended") || l.starts_with("long") {
        "extended".into()
    } else if l.starts_with("conveyance") {
        "conveyance".into()
    } else {
        raw.trim().to_string()
    }
}

fn ata_self_test(v: &Value) -> (Option<SelfTest>, bool) {
    let running = u(path(v, &["ata_smart_data", "self_test", "status", "value"]))
        .is_some_and(|s| s >> 4 == 0xF);
    let newest = path(v, &["ata_smart_self_test_log", "standard", "table"])
        .and_then(Value::as_array)
        .and_then(|t| t.first());
    let test = newest.map(|t| {
        let status = u(path(t, &["status", "value"])).unwrap_or(0);
        // High nibble: 0 ok, 1/2 aborted or interrupted by the host, 3–8 failed, 15 running.
        let passed = match status >> 4 {
            0 => Some(true),
            3..=8 => Some(false),
            _ => None,
        };
        SelfTest {
            kind: test_kind(&s(path(t, &["type", "string"])).unwrap_or_default()),
            passed,
            result: s(path(t, &["status", "string"])).unwrap_or_default(),
            hours: u(t.get("lifetime_hours")),
        }
    });
    (test, running)
}

fn nvme_self_test(v: &Value) -> (Option<SelfTest>, bool) {
    let log = v.get("nvme_self_test_log");
    let running = log
        .and_then(|l| u(path(l, &["current_self_test_operation", "value"])))
        .is_some_and(|op| op != 0);
    let newest = log
        .and_then(|l| l.get("table"))
        .and_then(Value::as_array)
        .and_then(|t| t.first());
    let test = newest.map(|t| {
        let result = u(path(t, &["self_test_result", "value"])).unwrap_or(0);
        // 0 ok; 1–4, 8, 9 aborted; 5–7 failed.
        let passed = match result {
            0 => Some(true),
            5..=7 => Some(false),
            _ => None,
        };
        SelfTest {
            kind: test_kind(&s(path(t, &["self_test_code", "string"])).unwrap_or_default()),
            passed,
            result: s(path(t, &["self_test_result", "string"])).unwrap_or_default(),
            hours: u(t.get("power_on_hours")),
        }
    });
    (test, running)
}

/// Does the output carry any SMART data at all (not just a failed open)?
pub fn has_data(v: &Value) -> bool {
    v.get("smart_status").is_some()
        || v.get("ata_smart_attributes").is_some()
        || v.get("nvme_smart_health_information_log").is_some()
        || v.get("scsi_error_counter_log").is_some()
        || v.get("temperature").is_some()
}

/// Was the disk asleep (`-n standby`)?
pub fn in_standby(v: &Value) -> bool {
    path(v, &["smartctl", "messages"])
        .and_then(Value::as_array)
        .is_some_and(|msgs| {
            msgs.iter().any(|m| {
                m.get("string")
                    .and_then(Value::as_str)
                    .is_some_and(|t| t.contains("STANDBY") || t.contains("SLEEP"))
            })
        })
}

/// First error message smartctl printed.
pub fn error_message(v: &Value) -> Option<String> {
    path(v, &["smartctl", "messages"])
        .and_then(Value::as_array)?
        .iter()
        .find(|m| m.get("severity").and_then(Value::as_str) == Some("error"))
        .and_then(|m| s(m.get("string")))
}

/// Does the device support SMART at all?
pub fn supported(v: &Value) -> bool {
    path(v, &["smart_support", "available"]).and_then(Value::as_bool) != Some(false)
}

pub fn parse(v: &Value, checked: i64) -> Smart {
    let protocol = match path(v, &["device", "protocol"]).and_then(Value::as_str) {
        Some("ATA") => Protocol::Ata,
        Some("SCSI") => Protocol::Scsi,
        Some("NVMe") => Protocol::Nvme,
        _ => Protocol::Other,
    };
    let nvme = v.get("nvme_smart_health_information_log");
    let nv = |key: &str| nvme.and_then(|n| u(n.get(key)));
    let (self_test, self_test_running) = match protocol {
        Protocol::Nvme => nvme_self_test(v),
        Protocol::Ata => ata_self_test(v),
        _ => (None, false),
    };
    let scsi_uncorrected = v.get("scsi_error_counter_log").map(|log| {
        ["read", "write", "verify"]
            .iter()
            .filter_map(|k| u(path(log, &[k, "total_uncorrected_errors"])))
            .sum::<u64>()
    });
    let temp = path(v, &["temperature", "current"])
        .and_then(Value::as_i64)
        .or_else(|| {
            nv("temperature")
                .map(|t| t as i64)
                .or_else(|| ata_attr(v, 194).map(|r| (r & 0xFF) as i64))
        })
        .map(|t| t as i32);
    Smart {
        checked,
        protocol,
        model: s(v.get("model_name")).or_else(|| s(v.get("scsi_model_name"))),
        serial: s(v.get("serial_number")),
        passed: path(v, &["smart_status", "passed"]).and_then(Value::as_bool),
        temp,
        power_on_hours: u(path(v, &["power_on_time", "hours"])).or_else(|| nv("power_on_hours")),
        realloc: match protocol {
            Protocol::Ata => ata_attr(v, 5),
            Protocol::Scsi => u(v.get("scsi_grown_defect_list")),
            _ => None,
        },
        pending: (protocol == Protocol::Ata)
            .then(|| ata_attr(v, 197))
            .flatten(),
        uncorrectable: match protocol {
            Protocol::Ata => ata_attr(v, 198),
            Protocol::Scsi => scsi_uncorrected,
            _ => None,
        },
        crc: (protocol == Protocol::Ata)
            .then(|| ata_attr(v, 199))
            .flatten(),
        wear: nv("percentage_used").or_else(|| u(path(v, &["endurance_used", "current_percent"]))),
        nvme_critical_warning: nv("critical_warning"),
        media_errors: nv("media_errors"),
        available_spare: nv("available_spare"),
        self_test,
        self_test_running,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Value {
        let bytes =
            std::fs::read(format!("{}/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn ata_hdd() {
        let v = fixture("smartctl-ata.json");
        assert!(has_data(&v) && supported(&v) && !in_standby(&v));
        let s = parse(&v, 1000);
        assert_eq!(s.protocol, Protocol::Ata);
        assert_eq!(s.model.as_deref(), Some("WDC WD30EFRX-68AX9N0"));
        assert_eq!(s.serial.as_deref(), Some("WD-WMEXAMPLE0001"));
        assert_eq!(s.passed, Some(true));
        assert_eq!(s.temp, Some(34));
        assert_eq!(s.power_on_hours, Some(61_234));
        assert_eq!(
            (s.realloc, s.pending, s.uncorrectable, s.crc),
            (Some(0), Some(0), Some(0), Some(2))
        );
        assert_eq!(s.wear, None);
        let t = s.self_test.as_ref().unwrap();
        assert_eq!((t.kind.as_str(), t.passed), ("short", Some(true)));
        assert_eq!(t.hours, Some(61_200));
        assert_eq!(s.self_test_time(), Some(1000 - 34 * 3600));
        assert!(!s.self_test_running);
    }

    #[test]
    fn ata_failing_disk() {
        let v = fixture("smartctl-ata-failing.json");
        let s = parse(&v, 0);
        assert_eq!(s.passed, Some(false));
        assert_eq!((s.realloc, s.pending), (Some(184), Some(16)));
        let t = s.self_test.unwrap();
        assert_eq!((t.kind.as_str(), t.passed), ("extended", Some(false)));
        assert_eq!(t.result, "Completed: read failure");
    }

    #[test]
    fn nvme() {
        let v = fixture("smartctl-nvme.json");
        let s = parse(&v, 0);
        assert_eq!(s.protocol, Protocol::Nvme);
        assert_eq!(s.serial.as_deref(), Some("NVEXAMPLE0003"));
        assert_eq!(s.temp, Some(25));
        assert_eq!(s.power_on_hours, Some(834));
        assert_eq!(
            (
                s.wear,
                s.nvme_critical_warning,
                s.media_errors,
                s.available_spare
            ),
            (Some(6), Some(0), Some(0), Some(100))
        );
        assert_eq!((s.realloc, s.crc), (None, None));
        assert!(s.self_test.is_none() && !s.self_test_running);
    }

    #[test]
    fn standby_and_open_failure() {
        let v = fixture("smartctl-standby.json");
        assert!(in_standby(&v) && !has_data(&v));
        let v: Value = serde_json::from_str(
            r#"{"smartctl":{"exit_status":2,"messages":[{"string":"Smartctl open device: /dev/sdx failed: No such device","severity":"error"}]}}"#,
        )
        .unwrap();
        assert!(!has_data(&v) && !in_standby(&v));
        assert_eq!(
            error_message(&v).as_deref(),
            Some("Smartctl open device: /dev/sdx failed: No such device")
        );
    }
}
