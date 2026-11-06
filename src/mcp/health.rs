//! Health tools (roles in `[mcp] health_roles`): the dashboard's data, read-only, from the
//! monitor's latest snapshot. Built on the status API document so both stay in step.

use super::tools::{Call, Out, ToolError, ToolResult, blocking, time};
use crate::health::alerts::LogEntry;
use crate::web::monitor::alert_lang;
use crate::web::status;
use serde::Deserialize;
use serde_json::{Value, json};

/// The status API document (without alert channels) as JSON.
fn document(c: &Call) -> Value {
    let now = crate::web::now();
    let snap = c.st.health.latest();
    serde_json::to_value(status::build(&c.st, snap.as_deref(), Vec::new(), now))
        .unwrap_or(Value::Null)
}

fn pick(doc: &Value, keys: &[&str]) -> Value {
    let mut out = serde_json::Map::new();
    for k in keys {
        out.insert(
            (*k).to_string(),
            doc.get(*k).cloned().unwrap_or(Value::Null),
        );
    }
    Value::Object(out)
}

fn no_data() -> ToolError {
    ToolError::new(
        "no_data",
        "No health data yet; the monitor takes its first reading a minute after start.",
    )
}

pub fn summary(c: &Call) -> ToolResult {
    let doc = document(c);
    let mut v = pick(
        &doc,
        &[
            "verdict", "as_of", "problems", "alerts", "pools", "md", "host",
        ],
    );
    let now = crate::web::now();
    v["data_age_secs"] = doc
        .get("as_of")
        .and_then(Value::as_i64)
        .map_or(Value::Null, |t| json!(now - t));
    v["as_of"] = doc
        .get("as_of")
        .and_then(Value::as_i64)
        .map_or(Value::Null, time);
    // Disks in short; the disk tool has the details.
    v["disks"] = Value::Array(
        doc.get("disks")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|d| {
                pick(
                    d,
                    &[
                        "name", "label", "kind", "model", "size", "pools", "md", "smart", "passed",
                        "temp", "wear", "checked",
                    ],
                )
            })
            .collect(),
    );
    Ok(Out::json(v))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlertsArgs {
    limit: Option<usize>,
}

pub async fn alerts(c: &Call, a: AlertsArgs) -> ToolResult {
    let limit = a.limit.unwrap_or(50).clamp(1, 500);
    let db = c.st.db.clone();
    let log: Vec<LogEntry> = blocking(move || Ok(db.alert_log(limit)?)).await?;
    let lang = alert_lang(&c.st);
    let doc = document(c);
    let recent: Vec<Value> = log
        .iter()
        .map(|e| {
            json!({
                "time": time(e.time),
                "key": e.key,
                "what": e.what,
                "severity": e.severity,
                "title": e.msg.render(&c.st.i18n, lang),
            })
        })
        .collect();
    Ok(Out::json(json!({
        "verdict": doc.get("verdict"),
        "active": doc.get("alerts"),
        "recent": recent,
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NameArgs {
    name: String,
}

pub fn pool(c: &Call, a: NameArgs) -> ToolResult {
    let snap = c.st.health.latest().ok_or_else(no_data)?;
    let pools = snap.pools.ok().ok_or_else(|| {
        ToolError::new(
            "no_data",
            "There is no ZFS pool data (no ZFS, or zpool failed).",
        )
    })?;
    let p = pools
        .iter()
        .find(|p| p.name == a.name.trim())
        .ok_or_else(|| {
            let names: Vec<&str> = pools.iter().map(|p| p.name.as_str()).collect();
            ToolError::new(
                "not_found",
                format!("No pool \"{}\"; pools: {}", a.name.trim(), names.join(", ")),
            )
        })?;
    let mut v = serde_json::to_value(p).unwrap_or(Value::Null);
    v["as_of"] = time(snap.taken);
    Ok(Out::json(v))
}

pub fn disk(c: &Call, a: NameArgs) -> ToolResult {
    let doc = document(c);
    let want = a.name.trim().trim_start_matches("/dev/");
    let disks = doc
        .get("disks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let found = disks.iter().find(|d| {
        ["name", "label", "serial"].iter().any(|k| {
            d.get(*k)
                .and_then(Value::as_str)
                .is_some_and(|s| s.eq_ignore_ascii_case(want))
        })
    });
    match found {
        Some(d) => {
            let mut d = d.clone();
            d["as_of"] = doc
                .get("as_of")
                .and_then(Value::as_i64)
                .map_or(Value::Null, time);
            Ok(Out::json(d))
        }
        None if c.st.health.latest().is_none() => Err(no_data()),
        None => {
            let names: Vec<&str> = disks
                .iter()
                .filter_map(|d| d.get("name").and_then(Value::as_str))
                .collect();
            Err(ToolError::new(
                "not_found",
                format!("No disk \"{want}\"; disks: {}", names.join(", ")),
            ))
        }
    }
}

pub fn datasets(c: &Call) -> ToolResult {
    let doc = document(c);
    if c.st.health.latest().is_none() {
        return Err(no_data());
    }
    Ok(Out::json(json!({
        "as_of": doc.get("as_of").and_then(Value::as_i64).map_or(Value::Null, time),
        "datasets": doc.get("datasets"),
    })))
}
