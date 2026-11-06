//! Share tools: the same rules as the share dialog (volumes reachable from the internet,
//! never above the role, always a password, an expiry). The agent owns its shares; the
//! admin sees them under all shares. Every share an agent creates raises an alert, because
//! it publishes files to the internet.

use super::files::IdArgs;
use super::tools::{Call, Out, ToolError, ToolResult, Touched, blocking, show, time};
use crate::auth::perm::Perms;
use crate::db::Share;
use crate::fs::path::RelPath;
use crate::health::alerts::{Msg, Severity};
use crate::shares::{self, Expiry, Mode, NewShare, Password, Pause, ShareError, Status};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateArgs {
    path: String,
    mode: Option<String>,
    expiry: Option<String>,
    password: Option<String>,
    #[serde(default)]
    note: String,
    max_file_mib: Option<u64>,
}

impl From<ShareError> for ToolError {
    fn from(e: ShareError) -> Self {
        match e {
            ShareError::NoVolume | ShareError::NotFound => ToolError::not_found(),
            ShareError::NotRemote => ToolError::new(
                "denied",
                "This volume isn't reachable from the internet, so it can't be shared.",
            ),
            ShareError::NoRights => ToolError::denied("share this (with this mode)"),
            ShareError::UploadNeedsFolder => {
                ToolError::new("bad_arguments", "Upload modes need a folder.")
            }
            ShareError::PasswordTooShort => ToolError::new(
                "bad_arguments",
                format!(
                    "The password needs at least {} characters.",
                    shares::MIN_OWN_PASSWORD
                ),
            ),
            ShareError::NoteTooLong => ToolError::new("bad_arguments", "The note is too long."),
            ShareError::TooMany => ToolError::new("too_many", "This agent has too many shares."),
            ShareError::Db(e) => e.into(),
        }
    }
}

fn link(c: &Call, id: &str) -> String {
    format!("{}/s/{id}", c.st.cfg.server.public_url)
}

fn status_code(s: Status) -> &'static str {
    match s {
        Status::Paused(Pause::Owner) => "paused_owner",
        Status::Paused(Pause::Rights) => "paused_rights",
        Status::Paused(Pause::Volume) => "paused_volume",
        Status::Paused(Pause::Trash) => "paused_trash",
        other => other.code(),
    }
}

pub async fn create(c: &Call, a: CreateArgs, t: &mut Touched) -> ToolResult {
    let (vol, rel) = c.target(&a.path)?;
    let path = show(vol.name(), &rel);
    t.0.push(path.clone());
    if !c.perms(&vol, &rel).contains(Perms::READ) {
        return Err(ToolError::not_found());
    }
    let mode_s = a.mode.as_deref().unwrap_or("view");
    let mode = Mode::parse(mode_s)
        .ok_or_else(|| ToolError::new("bad_arguments", "mode is view, view_upload or upload."))?;
    let expiry_s = a.expiry.as_deref().unwrap_or("7d");
    let expiry = Expiry::parse(expiry_s)
        .ok_or_else(|| ToolError::new("bad_arguments", "expiry is 1d, 7d, 30d or never."))?;
    let password = match a.password {
        Some(p) => Password::Own(p),
        None => Password::Passphrase(c.st.words.passphrase(&c.st.cfg.alerts.language)),
    };
    let max_file = a
        .max_file_mib
        .map(|m| {
            m.checked_mul(1 << 20)
                .ok_or_else(|| ToolError::new("bad_arguments", "max_file_mib is too large."))
        })
        .transpose()?;
    let (st, key, vname, note, pw) = (
        c.st.clone(),
        c.agent.key.clone(),
        vol.name().to_string(),
        a.note,
        password.clone(),
    );
    let share = blocking(move || {
        let owner = st
            .db
            .user(&key)?
            .filter(|u| u.is_agent())
            .ok_or_else(|| ToolError::denied("share"))?;
        Ok(shares::create(
            &st.cfg,
            &st.volumes,
            &st.db,
            &NewShare {
                owner: &owner,
                volume: &vname,
                path: &rel,
                mode,
                expiry,
                password: &pw,
                note: &note,
                max_file,
            },
            crate::web::now(),
        )?)
    })
    .await?;
    t.0.push(format!("share:{}", share.id));
    if c.st.cfg.mcp.notify_shares {
        let msg = match share.expires {
            Some(until) => Msg::new("agent_share").text(
                "until",
                jiff::Timestamp::from_second(until)
                    .map(|ts| {
                        ts.to_zoned(c.st.cfg.server.timezone.clone())
                            .strftime("%Y-%m-%d %H:%M")
                            .to_string()
                    })
                    .unwrap_or_default(),
            ),
            None => Msg::new("agent_share_forever"),
        }
        .text("agent", &c.agent.name)
        .text("path", &path)
        .text("mode", &share.mode)
        .text("link", link(c, &share.id));
        crate::web::raise(
            &c.st,
            &format!("agent_share:{}", share.id),
            0,
            Severity::Warning,
            msg,
        );
    }
    Ok(Out::json(json!({
        "id": share.id,
        "link": link(c, &share.id),
        "password": password.text(),
        "path": path,
        "mode": share.mode,
        "expires": share.expires.map(time),
        "note": "The password is shown only now. Send it to the recipient separately from the link.",
    })))
}

fn share_json(c: &Call, s: &Share, status: Status) -> Value {
    let rel = RelPath::parse(&s.path).unwrap_or_default();
    json!({
        "id": s.id,
        "link": link(c, &s.id),
        "path": show(&s.volume, &rel),
        "type": if s.is_dir { "folder" } else { "file" },
        "mode": s.mode,
        "status": status_code(status),
        "created": time(s.created),
        "expires": s.expires.map(time),
        "opens": s.opens,
        "downloads": s.downloads,
        "uploads": s.uploads,
        "last_opened": s.last_opened.map(time),
        "note": s.note,
    })
}

pub async fn list(c: &Call) -> ToolResult {
    let (st, key) = (c.st.clone(), c.agent.key.clone());
    let mut found = blocking(move || {
        let now = crate::web::now();
        let mut out = Vec::new();
        for s in st.db.shares()?.into_iter().filter(|s| s.owner == key) {
            let (status, _) = shares::status(&st.cfg, &st.volumes, &st.db, &s, now)?;
            out.push((s, status));
        }
        Ok(out)
    })
    .await?;
    found.sort_by_key(|(s, _)| std::cmp::Reverse(s.created));
    let list: Vec<Value> = found.iter().map(|(s, st)| share_json(c, s, *st)).collect();
    Ok(Out::json(json!({ "shares": list })))
}

pub async fn revoke(c: &Call, a: IdArgs, t: &mut Touched) -> ToolResult {
    let id = a.id.trim().to_string();
    t.0.push(format!("share:{id}"));
    if !shares::plausible_id(&id) {
        return Err(ToolError::new("not_found", "No such share of this agent."));
    }
    let (st, key) = (c.st.clone(), c.agent.key.clone());
    let done = blocking(move || {
        let Some(s) = st.db.share(&id)?.filter(|s| s.owner == key) else {
            return Ok(None);
        };
        if s.revoked.is_some() {
            return Ok(Some((s, false)));
        }
        st.db
            .update_share(&id, true, |s| s.revoked = Some(crate::web::now()))?;
        shares::end_guests(&st.db, &shares::share_scope(&id))?;
        Ok(Some((s, true)))
    })
    .await?;
    match done {
        Some((s, now_revoked)) => {
            let rel = RelPath::parse(&s.path).unwrap_or_default();
            t.0.push(show(&s.volume, &rel));
            Ok(Out::json(
                json!({ "id": s.id, "revoked": true, "already": !now_revoked }),
            ))
        }
        None => Err(ToolError::new("not_found", "No such share of this agent.")),
    }
}
