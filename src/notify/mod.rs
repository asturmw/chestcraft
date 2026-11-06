//! Alert channels: Telegram and ntfy, each optional. Messages go through `/usr/bin/curl`
//! with everything, including the URL with the bot token, in a curl config on stdin, so no
//! secret shows up in a process list. A delivery that fails is tried again after 1, 5, 15
//! and 60 minutes. The queue lives in memory; a critical alert repeats every 24 hours
//! anyway. Per channel the database keeps the last delivery and the last error.

use crate::config::Config;
use crate::config::secrets::Secrets;
use crate::db::Db;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::Instant;

const CURL_TIMEOUT_SECS: u64 = 20;
const RETRY_AFTER: [u64; 4] = [60, 300, 900, 3600];
const MAX_QUEUE: usize = 500;
pub const TELEGRAM_API: &str = "https://api.telegram.org";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Info,
    Warning,
    Critical,
    Resolved,
}

impl Level {
    fn icon(self) -> &'static str {
        match self {
            Level::Info => "ℹ️",
            Level::Warning => "🟠",
            Level::Critical => "🔴",
            Level::Resolved => "✅",
        }
    }

    fn ntfy_priority(self) -> u8 {
        match self {
            Level::Critical => 5,
            Level::Warning => 4,
            Level::Info | Level::Resolved => 3,
        }
    }

    fn ntfy_tag(self) -> &'static str {
        match self {
            Level::Info => "information_source",
            Level::Warning => "warning",
            Level::Critical => "rotating_light",
            Level::Resolved => "white_check_mark",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub level: Level,
    pub title: String,
    /// More lines below the title (may be empty).
    pub body: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Telegram,
    Ntfy,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Telegram => "telegram",
            Kind::Ntfy => "ntfy",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        match s {
            "telegram" => Some(Kind::Telegram),
            "ntfy" => Some(Kind::Ntfy),
            _ => None,
        }
    }
}

/// A configured channel. `problem` is set when it can't work (token missing).
#[derive(Clone)]
pub struct Channel {
    pub kind: Kind,
    target: Target,
    pub problem: Option<String>,
}

#[derive(Clone)]
enum Target {
    Telegram {
        chat_id: i64,
        token: Option<String>,
    },
    Ntfy {
        server: String,
        topic: String,
        token: Option<String>,
    },
}

impl Channel {
    pub fn telegram(chat_id: i64, token: String) -> Channel {
        Channel {
            kind: Kind::Telegram,
            target: Target::Telegram {
                chat_id,
                token: Some(token),
            },
            problem: None,
        }
    }

    /// Send one message now, without recording a status.
    pub async fn send(&self, msg: &Message) -> Result<(), String> {
        deliver(self, msg).await
    }

    /// Where messages go, without secrets: "chat 12345", "ntfy.sh/topic".
    pub fn destination(&self) -> String {
        match &self.target {
            Target::Telegram { chat_id, .. } => format!("chat {chat_id}"),
            Target::Ntfy { server, topic, .. } => {
                let host = server
                    .split_once("://")
                    .map(|(_, h)| h)
                    .unwrap_or(server.as_str());
                format!("{host}/{topic}")
            }
        }
    }
}

/// Delivery status of one channel, kept in the database.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Status {
    pub last_ok: Option<i64>,
    pub last_error: Option<String>,
    pub last_error_at: Option<i64>,
}

struct Pending {
    channel: usize,
    msg: Message,
    attempts: usize,
    due: Instant,
}

pub struct Notifier {
    channels: Vec<Channel>,
    queue: Mutex<VecDeque<Pending>>,
    wake: tokio::sync::Notify,
    db: Arc<Db>,
    /// Every message handed to `send`, for tests.
    #[cfg(test)]
    pub sent: Mutex<Vec<Message>>,
}

impl Notifier {
    pub fn new(cfg: &Config, secrets: &Secrets, db: Arc<Db>) -> Notifier {
        let mut channels = Vec::new();
        if let Some(t) = &cfg.alerts.telegram {
            channels.push(Channel {
                kind: Kind::Telegram,
                problem: secrets
                    .telegram_token
                    .is_none()
                    .then(|| "telegram_token is missing from the secrets file".to_string()),
                target: Target::Telegram {
                    chat_id: t.chat_id,
                    token: secrets.telegram_token.clone(),
                },
            });
        }
        if let Some(n) = &cfg.alerts.ntfy {
            channels.push(Channel {
                kind: Kind::Ntfy,
                problem: None,
                target: Target::Ntfy {
                    server: n.server.clone(),
                    topic: n.topic.clone(),
                    token: secrets.ntfy_token.clone(),
                },
            });
        }
        Notifier {
            channels,
            queue: Mutex::new(VecDeque::new()),
            wake: tokio::sync::Notify::new(),
            db,
            #[cfg(test)]
            sent: Mutex::new(Vec::new()),
        }
    }

    pub fn channels(&self) -> &[Channel] {
        &self.channels
    }

    pub fn status(&self, kind: Kind) -> Status {
        self.db
            .channel_status(kind.name())
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    /// Queue `msg` for every channel.
    pub fn send(&self, msg: Message) {
        #[cfg(test)]
        self.sent.lock().unwrap().push(msg.clone());
        let mut q = self.queue.lock().unwrap();
        for (i, c) in self.channels.iter().enumerate() {
            if c.problem.is_some() {
                continue;
            }
            if q.len() >= MAX_QUEUE {
                tracing::warn!("alert queue is full; dropping a message");
                break;
            }
            q.push_back(Pending {
                channel: i,
                msg: msg.clone(),
                attempts: 0,
                due: Instant::now(),
            });
        }
        drop(q);
        self.wake.notify_one();
    }

    /// Send right away (the test button); records the status like any delivery.
    pub async fn send_now(&self, kind: Kind, msg: &Message) -> Result<(), String> {
        let c = self
            .channels
            .iter()
            .find(|c| c.kind == kind)
            .ok_or_else(|| format!("{} is not configured", kind.name()))?;
        if let Some(p) = &c.problem {
            return Err(p.clone());
        }
        let result = deliver(c, msg).await;
        self.record(c.kind, &result).await;
        result
    }

    async fn record(&self, kind: Kind, result: &Result<(), String>) {
        let db = self.db.clone();
        let result = result.clone();
        let now = jiff::Timestamp::now().as_second();
        let _ = tokio::task::spawn_blocking(move || {
            let mut s: Status = db
                .channel_status(kind.name())
                .ok()
                .flatten()
                .unwrap_or_default();
            match result {
                Ok(()) => s.last_ok = Some(now),
                Err(e) => {
                    s.last_error = Some(e);
                    s.last_error_at = Some(now);
                }
            }
            if let Err(e) = db.put_channel_status(kind.name(), &s) {
                tracing::warn!("can't store the alert channel status: {e}");
            }
        })
        .await;
    }

    /// Deliver queued messages until the process ends.
    pub async fn run(self: Arc<Self>) {
        loop {
            let next = {
                let mut q = self.queue.lock().unwrap();
                let now = Instant::now();
                match q.iter().position(|p| p.due <= now) {
                    Some(i) => Ok(q.remove(i).expect("index from position")),
                    None => Err(q.iter().map(|p| p.due).min()),
                }
            };
            let mut p = match next {
                Ok(p) => p,
                Err(Some(due)) => {
                    tokio::select! {
                        _ = tokio::time::sleep_until(due) => {}
                        _ = self.wake.notified() => {}
                    }
                    continue;
                }
                Err(None) => {
                    self.wake.notified().await;
                    continue;
                }
            };
            let channel = &self.channels[p.channel];
            let result = deliver(channel, &p.msg).await;
            self.record(channel.kind, &result).await;
            if let Err(e) = result {
                tracing::warn!(channel = channel.kind.name(), "alert delivery failed: {e}");
                if let Some(wait) = RETRY_AFTER.get(p.attempts) {
                    p.attempts += 1;
                    p.due = Instant::now() + Duration::from_secs(*wait);
                    self.queue.lock().unwrap().push_back(p);
                } else {
                    tracing::error!(channel = channel.kind.name(), title = %p.msg.title, "giving up on an alert");
                }
            }
        }
    }
}

/// Text of a message as Telegram shows it.
pub fn telegram_text(msg: &Message) -> String {
    let mut t = format!("{} {}", msg.level.icon(), msg.title);
    if !msg.body.is_empty() {
        t.push('\n');
        t.push_str(&msg.body);
    }
    t
}

/// A string for a curl config file: quoted, with `\` `"` and control characters escaped.
pub fn curl_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A curl config for one POST of a JSON body.
pub fn curl_config(url: &str, protocols: &str, headers: &[String], json: &str) -> String {
    let mut c = String::new();
    c.push_str("silent\nshow-error\n");
    c.push_str(&format!("max-time = {CURL_TIMEOUT_SECS}\n"));
    c.push_str(&format!("proto = {}\n", curl_quote(protocols)));
    c.push_str(&format!("url = {}\n", curl_quote(url)));
    c.push_str(&format!(
        "header = {}\n",
        curl_quote("Content-Type: application/json")
    ));
    for h in headers {
        c.push_str(&format!("header = {}\n", curl_quote(h)));
    }
    c.push_str(&format!("data-raw = {}\n", curl_quote(json)));
    c.push_str(&format!("write-out = {}\n", curl_quote("\n%{http_code}")));
    c
}

/// A curl config for one GET.
pub fn curl_get_config(url: &str) -> String {
    format!(
        "silent\nshow-error\nmax-time = {CURL_TIMEOUT_SECS}\nproto = \"=https\"\nurl = {}\nwrite-out = {}\n",
        curl_quote(url),
        curl_quote("\n%{http_code}")
    )
}

/// Run curl with `config` on stdin; returns (HTTP status, body). `secret` is blanked out of
/// any error text.
pub async fn curl(config: String, secret: Option<&str>) -> Result<(u16, Vec<u8>), String> {
    let redact = |s: String| match secret {
        Some(t) if !t.is_empty() => s.replace(t, "<token>"),
        _ => s,
    };
    let mut child = tokio::process::Command::new(crate::tools::CURL)
        .args(["-q", "--config", "-"])
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("can't run {}: {e}", crate::tools::CURL))?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let io = async {
        stdin.write_all(config.as_bytes()).await?;
        drop(stdin);
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut out_r = (&mut stdout).take(1 << 20);
        let mut err_r = (&mut stderr).take(64 << 10);
        let (a, b) = tokio::join!(out_r.read_to_end(&mut out), err_r.read_to_end(&mut err));
        a?;
        b?;
        let status = child.wait().await?;
        Ok::<_, std::io::Error>((status, out, err))
    };
    let (status, out, err) = tokio::time::timeout(Duration::from_secs(CURL_TIMEOUT_SECS + 5), io)
        .await
        .map_err(|_| "curl didn't finish in time".to_string())?
        .map_err(|e| redact(format!("curl: {e}")))?;
    if !status.success() {
        let msg = String::from_utf8_lossy(&err).trim().to_string();
        return Err(redact(if msg.is_empty() {
            format!("curl exited with {status}")
        } else {
            msg
        }));
    }
    let text = out;
    let split = text.iter().rposition(|b| *b == b'\n').unwrap_or(0);
    let code = std::str::from_utf8(&text[split..])
        .ok()
        .and_then(|s| s.trim().parse::<u16>().ok())
        .unwrap_or(0);
    Ok((code, text[..split].to_vec()))
}

/// Telegram's answer: `{"ok":true,…}` or `{"ok":false,"description":"…"}`.
pub fn telegram_result(code: u16, body: &[u8]) -> Result<(), String> {
    let v: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
    if v.get("ok").and_then(|o| o.as_bool()) == Some(true) {
        return Ok(());
    }
    Err(v
        .get("description")
        .and_then(|d| d.as_str())
        .map(|d| format!("Telegram: {d}"))
        .unwrap_or_else(|| format!("Telegram answered HTTP {code}")))
}

fn ntfy_result(code: u16, body: &[u8]) -> Result<(), String> {
    if (200..300).contains(&code) {
        return Ok(());
    }
    let v: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
    Err(v
        .get("error")
        .and_then(|d| d.as_str())
        .map(|d| format!("ntfy: {d} (HTTP {code})"))
        .unwrap_or_else(|| format!("ntfy answered HTTP {code}")))
}

/// The curl config and the secret to blank out of errors for one message.
pub fn request(channel: &Channel, msg: &Message) -> Result<(String, Option<String>), String> {
    match &channel.target {
        Target::Telegram { chat_id, token } => {
            let token = token
                .as_ref()
                .ok_or_else(|| "telegram_token is missing".to_string())?;
            let body = serde_json::json!({
                "chat_id": chat_id,
                "text": telegram_text(msg),
                "disable_web_page_preview": true,
            });
            Ok((
                curl_config(
                    &format!("{TELEGRAM_API}/bot{token}/sendMessage"),
                    "=https",
                    &[],
                    &body.to_string(),
                ),
                Some(token.clone()),
            ))
        }
        Target::Ntfy {
            server,
            topic,
            token,
        } => {
            let mut body = serde_json::json!({
                "topic": topic,
                "title": msg.title,
                "message": if msg.body.is_empty() { msg.title.clone() } else { msg.body.clone() },
                "priority": msg.level.ntfy_priority(),
                "tags": [msg.level.ntfy_tag()],
            });
            if msg.body.is_empty()
                && let Some(o) = body.as_object_mut()
            {
                o.remove("title");
            }
            let headers: Vec<String> = token
                .iter()
                .map(|t| format!("Authorization: Bearer {t}"))
                .collect();
            Ok((
                curl_config(
                    &format!("{server}/"),
                    "=http,https",
                    &headers,
                    &body.to_string(),
                ),
                token.clone(),
            ))
        }
    }
}

async fn deliver(channel: &Channel, msg: &Message) -> Result<(), String> {
    let (config, secret) = request(channel, msg)?;
    let (code, body) = curl(config, secret.as_deref()).await?;
    match channel.kind {
        Kind::Telegram => telegram_result(code, &body),
        Kind::Ntfy => ntfy_result(code, &body),
    }
}

/// `chestcraft telegram-setup`: the chats that wrote to the bot, from `getUpdates`, as
/// (chat id, description).
pub async fn telegram_chats(token: &str) -> Result<Vec<(i64, String)>, String> {
    let (code, body) = curl(
        curl_get_config(&format!("{TELEGRAM_API}/bot{token}/getUpdates")),
        Some(token),
    )
    .await?;
    telegram_result(code, &body)?;
    let v: serde_json::Value = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
    Ok(parse_chats(&v))
}

pub fn parse_chats(v: &serde_json::Value) -> Vec<(i64, String)> {
    let mut out: Vec<(i64, String)> = Vec::new();
    for u in v
        .get("result")
        .and_then(|r| r.as_array())
        .into_iter()
        .flatten()
    {
        for kind in [
            "message",
            "my_chat_member",
            "channel_post",
            "edited_message",
        ] {
            let Some(chat) = u.get(kind).and_then(|m| m.get("chat")) else {
                continue;
            };
            let Some(id) = chat.get("id").and_then(|i| i.as_i64()) else {
                continue;
            };
            let s = |k: &str| chat.get(k).and_then(|x| x.as_str()).unwrap_or("");
            let name = [s("title"), s("first_name"), s("last_name")]
                .iter()
                .filter(|p| !p.is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join(" ");
            let user = s("username");
            let desc = format!(
                "{} {}{}",
                s("type"),
                name,
                if user.is_empty() {
                    String::new()
                } else {
                    format!(" (@{user})")
                }
            );
            if !out.iter().any(|(i, _)| *i == id) {
                out.push((id, desc.trim().to_string()));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(extra: &str) -> Config {
        Config::from_toml_str(&format!(
            "[server]\npublic_url = \"https://files.example.org\"\n{extra}"
        ))
        .unwrap()
    }

    const TOKEN: &str = "123456789:AAH-example_key_with_enough_length0";

    #[test]
    fn quoting_survives_hostile_text() {
        assert_eq!(curl_quote("a\"b\\c\nd\u{7}"), "\"a\\\"b\\\\c\\nd\"");
        let msg = Message {
            level: Level::Critical,
            title: "Pool \"tank\" is DEGRADED\nurl = \"https://evil\"".into(),
            body: "Bay 3 FAULTED".into(),
        };
        let channel = Channel {
            kind: Kind::Telegram,
            problem: None,
            target: Target::Telegram {
                chat_id: -100123,
                token: Some(TOKEN.into()),
            },
        };
        let (config, secret) = request(&channel, &msg).unwrap();
        assert_eq!(secret.as_deref(), Some(TOKEN));
        // One url line, and the title can't start a config line of its own.
        let lines: Vec<&str> = config.lines().collect();
        assert_eq!(lines.iter().filter(|l| l.starts_with("url")).count(), 1);
        assert!(
            lines
                .iter()
                .any(|l| *l == format!("url = \"{TELEGRAM_API}/bot{TOKEN}/sendMessage\""))
        );
        assert!(lines.iter().any(|l| l.starts_with("proto = \"=https\"")));
        let data = lines.iter().find(|l| l.starts_with("data-raw = ")).unwrap();
        assert!(data.contains("-100123") && data.contains("DEGRADED"));
    }

    #[test]
    fn ntfy_request() {
        let channel = Channel {
            kind: Kind::Ntfy,
            problem: None,
            target: Target::Ntfy {
                server: "http://ntfy.lan:8080".into(),
                topic: "nas-alerts".into(),
                token: Some("tk_secret".into()),
            },
        };
        assert_eq!(channel.destination(), "ntfy.lan:8080/nas-alerts");
        let (config, _) = request(
            &channel,
            &Message {
                level: Level::Warning,
                title: "Bay 2 is at 52 °C".into(),
                body: String::new(),
            },
        )
        .unwrap();
        assert!(config.contains("url = \"http://ntfy.lan:8080/\""));
        assert!(config.contains("header = \"Authorization: Bearer tk_secret\""));
        assert!(config.contains(r#"\"priority\":4"#), "{config}");
        assert!(config.contains("52 °C"));
    }

    #[test]
    fn answers() {
        assert!(telegram_result(200, br#"{"ok":true,"result":{}}"#).is_ok());
        assert_eq!(
            telegram_result(
                400,
                br#"{"ok":false,"error_code":400,"description":"Bad Request: chat not found"}"#
            )
            .unwrap_err(),
            "Telegram: Bad Request: chat not found"
        );
        assert!(telegram_result(502, b"<html>").unwrap_err().contains("502"));
        assert!(ntfy_result(200, b"{}").is_ok());
        assert!(
            ntfy_result(403, br#"{"code":40301,"http":403,"error":"forbidden"}"#)
                .unwrap_err()
                .contains("forbidden")
        );
        let chats = parse_chats(&serde_json::json!({"ok":true,"result":[
            {"update_id":1,"message":{"chat":{"id":4242,"type":"private","first_name":"Alex","username":"alex_example"},"text":"/start"}},
            {"update_id":2,"message":{"chat":{"id":4242,"type":"private","first_name":"Alex"},"text":"hi"}},
            {"update_id":3,"my_chat_member":{"chat":{"id":-1001,"type":"group","title":"NAS"}}}
        ]}));
        assert_eq!(
            chats,
            [
                (4242, "private Alex (@alex_example)".to_string()),
                (-1001, "group NAS".to_string())
            ]
        );
    }

    #[tokio::test]
    async fn channels_from_config_and_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Db::open(&dir.path().join("t.redb")).unwrap());
        let c = cfg("[alerts.telegram]\nchat_id = 42\n[alerts.ntfy]\ntopic = \"x\"\n");
        let n = Notifier::new(&c, &Secrets::default(), db.clone());
        assert_eq!(n.channels().len(), 2);
        assert!(n.channels()[0].problem.is_some());
        assert!(n.channels()[1].problem.is_none());
        let err = n
            .send_now(
                Kind::Telegram,
                &Message {
                    level: Level::Info,
                    title: "t".into(),
                    body: String::new(),
                },
            )
            .await
            .unwrap_err();
        assert!(err.contains("telegram_token"));
        // Only the working channel gets queued.
        n.send(Message {
            level: Level::Info,
            title: "t".into(),
            body: String::new(),
        });
        assert_eq!(n.queue.lock().unwrap().len(), 1);
        let none = Notifier::new(&cfg(""), &Secrets::default(), db);
        assert!(none.channels().is_empty());
        let first = n.sent.lock().unwrap()[0].clone();
        assert!(
            none.send_now(Kind::Ntfy, &first)
                .await
                .unwrap_err()
                .contains("not configured")
        );
    }
}
