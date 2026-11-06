//! Browse, read and write tools: the same checks as the web and WebDAV, as the agent's
//! role from the LAN. Writes never overwrite (keep both or skip), deleting goes to the
//! trash, and agents never see dot-names.

use super::tools::{Call, Out, ToolError, ToolResult, Touched, blocking, show, time};
use crate::auth::perm::{Perms, volume_perms};
use crate::fs::filetype::thumb_source;
use crate::fs::names::split_ext;
use crate::fs::path::{RelPath, validate_new_name};
use crate::fs::sort::natural_cmp;
use crate::fs::thumbs::{Size, ThumbError};
use crate::fs::transfer::{self, CopySpec, NewFile, OnClash, Placed};
use crate::fs::trash;
use crate::fs::volume::{Entry, Kind, VolumeRoot};
use crate::fs::{ops, ops::OpError};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use rmcp::model::ContentBlock;
use serde::Deserialize;
use serde_json::{Value, json};
use std::cmp::Ordering;
use std::io;
use std::os::unix::fs::FileExt;
use std::sync::Arc;

const DEFAULT_LIMIT: usize = 200;
const MAX_LIMIT: usize = 1000;

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Clash {
    #[default]
    KeepBoth,
    Skip,
}

impl Clash {
    fn on_clash(self) -> OnClash {
        match self {
            Clash::KeepBoth => OnClash::KeepBoth,
            Clash::Skip => OnClash::Skip,
        }
    }
}

fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::Dir => "folder",
        Kind::File => "file",
        Kind::Other => "other",
    }
}

fn entry_json(e: &Entry) -> Value {
    let mut v = json!({
        "name": e.name,
        "type": kind_name(e.meta.kind),
        "modified": time(e.meta.mtime),
    });
    if e.meta.kind == Kind::File {
        v["size"] = json!(e.meta.size);
    }
    v
}

fn io_err(e: io::Error) -> ToolError {
    OpError::from(e).into()
}

/// The folder at `rel`, which the agent may read.
fn readable_dir(c: &Call, vol: &VolumeRoot, rel: &RelPath) -> Result<(), ToolError> {
    if !c.perms(vol, rel).contains(Perms::READ) {
        return Err(ToolError::not_found());
    }
    match vol.stat(rel).map_err(io_err)?.kind {
        Kind::Dir => Ok(()),
        _ => Err(ToolError::new("not_a_folder", "That is not a folder.")),
    }
}

// ---------------------------------------------------------------------------------------
// volumes, list, stat, find

pub async fn volumes(c: &Call) -> ToolResult {
    let principal = c.principal();
    let st = c.st.clone();
    let vols: Vec<Arc<VolumeRoot>> = st
        .volumes
        .iter()
        .filter(|v| c.can_read_volume(v.name()))
        .cloned()
        .collect();
    blocking(move || {
        let list: Vec<Value> = vols
            .iter()
            .map(|v| {
                let perms = volume_perms(
                    &st.cfg,
                    &principal,
                    crate::auth::perm::Origin::Local,
                    v.name(),
                );
                let space = v.space().ok();
                let note = v
                    .cfg
                    .note
                    .as_ref()
                    .and_then(|n| n.en.clone().or(n.de.clone()));
                json!({
                    "name": v.name(),
                    "path": format!("/{}", v.name()),
                    "perms": perms.letters(),
                    "read_only": v.cfg.read_only,
                    "remote": v.cfg.remote,
                    "public": v.cfg.public,
                    "free_bytes": space.map(|s| s.avail),
                    "total_bytes": space.map(|s| s.total),
                    "note": note,
                })
            })
            .collect();
        Ok(Out::json(json!({ "volumes": list })))
    })
    .await
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SortBy {
    #[default]
    Name,
    Modified,
    Size,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SortOrder {
    #[default]
    Asc,
    Desc,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListArgs {
    path: String,
    #[serde(default)]
    offset: usize,
    limit: Option<usize>,
    #[serde(default)]
    sort: SortBy,
    #[serde(default)]
    order: SortOrder,
}

/// Folders first, then by the chosen key (names in natural order break ties).
fn sort_entries(entries: &mut [Entry], by: SortBy, order: SortOrder) {
    entries.sort_by(|a, b| {
        let dirs = (b.meta.kind == Kind::Dir).cmp(&(a.meta.kind == Kind::Dir));
        let key = match by {
            SortBy::Name => Ordering::Equal,
            SortBy::Modified => a.meta.mtime.cmp(&b.meta.mtime),
            SortBy::Size => a.meta.size.cmp(&b.meta.size),
        };
        let key = key.then_with(|| natural_cmp(&a.name, &b.name));
        dirs.then(if order == SortOrder::Desc {
            key.reverse()
        } else {
            key
        })
    });
}

fn page(entries: &[Entry], offset: usize, limit: Option<usize>) -> (Vec<Value>, Option<usize>) {
    let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let end = offset.saturating_add(limit).min(entries.len());
    let shown = entries
        .get(offset.min(entries.len())..end)
        .unwrap_or_default()
        .iter()
        .map(entry_json)
        .collect();
    (shown, (end < entries.len()).then_some(end))
}

pub async fn list(c: &Call, a: ListArgs, t: &mut Touched) -> ToolResult {
    if a.path.trim().trim_matches('/').is_empty() {
        return volumes(c).await;
    }
    let (vol, rel) = c.target(&a.path)?;
    t.0.push(show(vol.name(), &rel));
    readable_dir(c, &vol, &rel)?;
    let perms = c.perms(&vol, &rel);
    let (vol2, rel2) = (vol.clone(), rel.clone());
    let mut entries = blocking(move || vol2.list(&rel2, false).map_err(io_err)).await?;
    sort_entries(&mut entries, a.sort, a.order);
    let (shown, next) = page(&entries, a.offset, a.limit);
    Ok(Out::json(json!({
        "path": show(vol.name(), &rel),
        "perms": perms.letters(),
        "total": entries.len(),
        "offset": a.offset,
        "entries": shown,
        "next_offset": next,
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathArgs {
    path: String,
}

pub async fn stat(c: &Call, a: PathArgs, t: &mut Touched) -> ToolResult {
    let (vol, rel) = c.target(&a.path)?;
    t.0.push(show(vol.name(), &rel));
    let perms = c.perms(&vol, &rel);
    if !perms.contains(Perms::READ) {
        return Err(ToolError::not_found());
    }
    let (vol2, rel2) = (vol.clone(), rel.clone());
    let meta = blocking(move || vol2.stat(&rel2).map_err(io_err)).await?;
    let mut v = json!({
        "path": show(vol.name(), &rel),
        "name": rel.name().unwrap_or(vol.name()),
        "type": kind_name(meta.kind),
        "modified": time(meta.mtime),
        "perms": perms.letters(),
    });
    if meta.kind == Kind::File {
        v["size"] = json!(meta.size);
        v["mime"] = json!(
            mime_guess::from_path(rel.name().unwrap_or_default())
                .first_or_octet_stream()
                .essence_str()
        );
    }
    Ok(Out::json(v))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindArgs {
    path: String,
    pattern: String,
    limit: Option<usize>,
}

/// Case-insensitive match: `*` and `?` wildcards over the whole name, else a substring.
pub fn name_matches(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let n: Vec<char> = name.to_lowercase().chars().collect();
    if !p.iter().any(|c| *c == '*' || *c == '?') {
        let (p, n): (String, String) = (p.into_iter().collect(), n.into_iter().collect());
        return n.contains(&p);
    }
    // Iterative glob with backtracking to the last star.
    let (mut pi, mut ni) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ni));
            pi += 1;
        } else if let Some((sp, sn)) = star {
            pi = sp + 1;
            ni = sn + 1;
            star = Some((sp, sn + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

pub async fn find(c: &Call, a: FindArgs, t: &mut Touched) -> ToolResult {
    let (vol, rel) = c.target(&a.path)?;
    t.0.push(show(vol.name(), &rel));
    readable_dir(c, &vol, &rel)?;
    let (vol2, rel2) = (vol.clone(), rel.clone());
    let mut entries = blocking(move || vol2.list(&rel2, false).map_err(io_err)).await?;
    entries.retain(|e| name_matches(a.pattern.trim(), &e.name));
    sort_entries(&mut entries, SortBy::Name, SortOrder::Asc);
    let (shown, next) = page(&entries, 0, a.limit);
    Ok(Out::json(json!({
        "path": show(vol.name(), &rel),
        "pattern": a.pattern,
        "total": entries.len(),
        "entries": shown,
        "truncated": next.is_some(),
    })))
}

// ---------------------------------------------------------------------------------------
// read_file

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadArgs {
    path: String,
    #[serde(default)]
    offset: u64,
    length: Option<u64>,
    #[serde(default)]
    preview: bool,
}

/// Pictures MCP clients show as images.
fn image_mime(name: &str) -> Option<&'static str> {
    let (_, ext) = split_ext(name);
    Some(
        match ext.trim_start_matches('.').to_ascii_lowercase().as_str() {
            "png" => "image/png",
            "jpg" | "jpeg" | "jfif" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            _ => return None,
        },
    )
}

/// How much of `buf` (starting `skip` bytes in) is UTF-8 text, if it is text: no NUL
/// bytes, valid UTF-8 except for a sequence cut at the end of a piece that isn't the end
/// of the file.
fn text_part(buf: &[u8], at_eof: bool) -> Option<(usize, usize)> {
    if buf.contains(&0) {
        return None;
    }
    // A piece may start inside a character: skip its continuation bytes.
    let skip = buf
        .iter()
        .take(3)
        .take_while(|b| (**b & 0xC0) == 0x80)
        .count();
    let body = &buf[skip..];
    match std::str::from_utf8(body) {
        Ok(_) => Some((skip, body.len())),
        Err(e) if e.error_len().is_none() && !at_eof && e.valid_up_to() > 0 => {
            Some((skip, e.valid_up_to()))
        }
        Err(_) => None,
    }
}

pub async fn read_file(c: &Call, a: ReadArgs, t: &mut Touched) -> ToolResult {
    let (vol, rel) = c.target(&a.path)?;
    let path = show(vol.name(), &rel);
    t.0.push(path.clone());
    if !c.perms(&vol, &rel).contains(Perms::READ) {
        return Err(ToolError::not_found());
    }
    let name = rel.name().unwrap_or_default().to_string();
    let (vol2, rel2) = (vol.clone(), rel.clone());
    let (fd, meta) = blocking(move || {
        let fd = vol2.open_read(&rel2).map_err(io_err)?;
        let meta = VolumeRoot::meta(&fd).map_err(io_err)?;
        Ok((fd, meta))
    })
    .await?;
    if meta.kind != Kind::File {
        return Err(ToolError::new("not_a_file", "That is not a file."));
    }
    let max = c.st.cfg.mcp.max_read_bytes;

    if a.preview {
        let src = thumb_source(&name).ok_or_else(|| {
            ToolError::new(
                "no_preview",
                "There is no picture of this kind of file; read it without preview.",
            )
        })?;
        return match c.st.thumbs.get(fd, &meta, src, Size::View, || true).await {
            Ok(Some(thumb)) => {
                let bytes = thumb.bytes.len() as u64;
                Ok(Out {
                    json: json!({ "path": path, "size": meta.size, "preview": true,
                                  "mime": thumb.mime, "preview_bytes": bytes }),
                    extra: vec![ContentBlock::image(B64.encode(&thumb.bytes), thumb.mime)],
                    bytes,
                })
            }
            Ok(None) | Err(ThumbError::Unusable(_)) => Err(ToolError::new(
                "no_preview",
                "No picture could be made of this file.",
            )),
            Err(ThumbError::Skipped(why)) => Err(ToolError::new("no_preview", why)),
            Err(ThumbError::Busy) => Err(ToolError::new("busy", "Busy; try again in a moment.")),
            Err(ThumbError::Io(e)) => Err(io_err(e)),
        };
    }

    let whole_image = image_mime(&name).filter(|_| a.offset == 0 && a.length.is_none());
    if whole_image.is_some() && meta.size > max {
        return Err(ToolError::new(
            "too_large",
            format!(
                "The picture has {} bytes, more than the {max} one call returns; use preview=true \
                 for a smaller picture, or read it in pieces with offset and length (base64).",
                meta.size
            ),
        ));
    }
    let offset = a.offset.min(meta.size);
    let want = a.length.unwrap_or(max).min(max).min(meta.size - offset);
    let buf = blocking(move || {
        let file = std::fs::File::from(fd);
        let mut buf = vec![0u8; want as usize];
        let mut got = 0usize;
        while got < buf.len() {
            match file.read_at(&mut buf[got..], offset + got as u64) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(io_err(e)),
            }
        }
        buf.truncate(got);
        Ok(buf)
    })
    .await?;
    let end_of_piece = offset + buf.len() as u64;
    let at_eof = end_of_piece >= meta.size;
    let mime = mime_guess::from_path(&name)
        .first_or_octet_stream()
        .essence_str()
        .to_string();

    if let Some(img) = whole_image {
        let bytes = buf.len() as u64;
        return Ok(Out {
            json: json!({ "path": path, "size": meta.size, "encoding": "image", "mime": img }),
            extra: vec![ContentBlock::image(B64.encode(&buf), img)],
            bytes,
        });
    }
    let (start, len, encoding, block) = match text_part(&buf, at_eof) {
        Some((skip, len)) => {
            let text = String::from_utf8_lossy(&buf[skip..skip + len]).into_owned();
            (
                offset + skip as u64,
                len as u64,
                "text",
                ContentBlock::text(text),
            )
        }
        None => (
            offset,
            buf.len() as u64,
            "base64",
            ContentBlock::text(B64.encode(&buf)),
        ),
    };
    let next = start + len;
    Ok(Out {
        json: json!({
            "path": path,
            "size": meta.size,
            "offset": start,
            "length": len,
            "next_offset": (next < meta.size).then_some(next),
            "encoding": encoding,
            "mime": mime,
        }),
        extra: vec![block],
        bytes: len,
    })
}

// ---------------------------------------------------------------------------------------
// mkdir, write_file

/// Parent folder and new name of `rel`, with the name checked.
fn new_entry(rel: &RelPath) -> Result<(RelPath, String), ToolError> {
    let (Some(parent), Some(name)) = (rel.parent(), rel.name()) else {
        return Err(ToolError::new(
            "bad_path",
            "A volume itself can't be created or changed.",
        ));
    };
    validate_new_name(name).map_err(|e| ToolError::new("bad_path", format!("\"{name}\": {e}")))?;
    Ok((parent, name.to_string()))
}

fn parent_missing(e: ToolError) -> ToolError {
    if e.code == "not_found" {
        ToolError::new("not_found", "The folder it should go into doesn't exist.")
    } else {
        e
    }
}

pub async fn mkdir(c: &Call, a: PathArgs, t: &mut Touched) -> ToolResult {
    let (vol, rel) = c.target(&a.path)?;
    let path = show(vol.name(), &rel);
    t.0.push(path.clone());
    let (parent, name) = new_entry(&rel)?;
    if !c.perms(&vol, &rel).contains(Perms::WRITE) {
        return Err(ToolError::denied("create folders"));
    }
    let made = blocking(move || {
        let pfd = vol
            .open_dir(&parent)
            .map_err(|e| parent_missing(io_err(e)))?;
        match ops::mkdir_in(&pfd, &name) {
            Ok(()) => Ok(true),
            Err(OpError::Exists(x)) if x.is_dir => Ok(false),
            Err(e) => Err(e.into()),
        }
    })
    .await?;
    Ok(Out::json(
        json!({ "path": path, "created": made, "existed": !made }),
    ))
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Encoding {
    #[default]
    Text,
    Base64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriteArgs {
    path: String,
    content: String,
    #[serde(default)]
    encoding: Encoding,
    #[serde(default)]
    on_clash: Clash,
}

pub async fn write_file(c: &Call, a: WriteArgs, t: &mut Touched) -> ToolResult {
    let (vol, rel) = c.target(&a.path)?;
    t.0.push(show(vol.name(), &rel));
    let (parent, name) = new_entry(&rel)?;
    if !c.perms(&vol, &rel).contains(Perms::WRITE) {
        return Err(ToolError::denied("write files"));
    }
    let data = match a.encoding {
        Encoding::Text => a.content.into_bytes(),
        Encoding::Base64 => B64
            .decode(a.content.trim().as_bytes())
            .map_err(|e| ToolError::new("bad_arguments", format!("content isn't base64: {e}")))?,
    };
    let max = c.st.cfg.mcp.max_write_bytes;
    if data.len() as u64 > max {
        return Err(ToolError::new(
            "too_large",
            format!(
                "{} bytes is more than the {max} one call may write.",
                data.len()
            ),
        ));
    }
    let (db, actor, on_clash) = (c.st.db.clone(), c.agent.actor(), a.on_clash.on_clash());
    let size = data.len() as u64;
    let vol2 = vol.clone();
    let parent2 = parent.clone();
    let placed = blocking(move || {
        let now = crate::web::now();
        let mut file = NewFile::create(&db, &vol2, &parent2, &name, Some(size), now)
            .map_err(|e| parent_missing(e.into()))?;
        file.write_all(&data)?;
        Ok(file.commit(None, on_clash, &actor, now)?)
    })
    .await?;
    let asked = rel.name().unwrap_or_default().to_string();
    Ok(match placed {
        Placed::Done { name, .. } => {
            let at = parent.join(&name).map_err(|_| ToolError::failed())?;
            Out {
                json: json!({ "path": show(vol.name(), &at), "written": true, "size": size,
                              "renamed": name != asked }),
                extra: vec![],
                bytes: size,
            }
        }
        Placed::Skipped => Out::json(json!({ "path": show(vol.name(), &rel), "written": false,
                                             "skipped": "the name is taken" })),
    })
}

// ---------------------------------------------------------------------------------------
// rename, move, copy, delete

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenameArgs {
    path: String,
    new_name: String,
    #[serde(default)]
    on_clash: Clash,
}

fn placed_json(from: &str, vol: &str, parent: &RelPath, asked: &str, placed: Placed) -> Out {
    match placed {
        Placed::Done { name, .. } => {
            let at = parent
                .join(&name)
                .map(|p| show(vol, &p))
                .unwrap_or_default();
            Out::json(json!({ "from": from, "path": at, "done": true, "renamed": name != asked }))
        }
        Placed::Skipped => Out::json(json!({ "from": from, "done": false,
                                             "skipped": "the name is taken" })),
    }
}

pub async fn rename(c: &Call, a: RenameArgs, t: &mut Touched) -> ToolResult {
    let (vol, src) = c.target(&a.path)?;
    let from = show(vol.name(), &src);
    t.0.push(from.clone());
    let Some(parent) = src.parent() else {
        return Err(ToolError::new("bad_path", "A volume can't be renamed."));
    };
    let new_name = a.new_name.trim().to_string();
    let dst = parent
        .join(&new_name)
        .map_err(|e| ToolError::new("bad_path", format!("\"{new_name}\": {e}")))?;
    new_entry(&dst)?;
    t.0.push(show(vol.name(), &dst));
    if !c.perms(&vol, &src).contains(Perms::READ) {
        return Err(ToolError::not_found());
    }
    if !c.perms(&vol, &src).contains(Perms::MOVE) || !c.perms(&vol, &dst).contains(Perms::MOVE) {
        return Err(ToolError::denied("rename"));
    }
    if dst == src {
        return Err(ToolError::new("invalid", "That is its name already."));
    }
    let (db, actor, on_clash) = (c.st.db.clone(), c.agent.actor(), a.on_clash.on_clash());
    let (vol2, parent2, name2) = (vol.clone(), parent.clone(), new_name.clone());
    let placed = blocking(move || {
        Ok(transfer::move_entry(
            &db,
            &vol2,
            &src,
            &vol2,
            &parent2,
            &name2,
            on_clash,
            &actor,
            crate::web::now(),
        )?)
    })
    .await?;
    Ok(placed_json(&from, vol.name(), &parent, &new_name, placed))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoveArgs {
    path: String,
    to: String,
    #[serde(default)]
    on_clash: Clash,
}

/// `move` (is_move) or `copy` into the folder `to`, keeping the name.
pub async fn move_to(c: &Call, a: MoveArgs, t: &mut Touched, is_move: bool) -> ToolResult {
    let (svol, src) = c.target(&a.path)?;
    let from = show(svol.name(), &src);
    t.0.push(from.clone());
    let (dvol, dparent) = c.target(&a.to)?;
    t.0.push(show(dvol.name(), &dparent));
    let Some(name) = src.name().map(str::to_string) else {
        return Err(ToolError::new(
            "bad_path",
            "A whole volume can't be moved or copied.",
        ));
    };
    let sperms = c.perms(&svol, &src);
    if !sperms.contains(Perms::READ) {
        return Err(ToolError::not_found());
    }
    if is_move && !sperms.contains(Perms::MOVE) {
        return Err(ToolError::denied("move this"));
    }
    let dst = dparent.join(&name).map_err(|_| ToolError::failed())?;
    let needed = if is_move && svol.name() == dvol.name() {
        Perms::MOVE
    } else {
        Perms::WRITE
    };
    if !c.perms(&dvol, &dparent).contains(Perms::READ) {
        return Err(ToolError::new(
            "not_found",
            "The target folder doesn't exist.",
        ));
    }
    if !c.perms(&dvol, &dst).contains(needed) {
        return Err(ToolError::denied(if is_move {
            "move things into that folder"
        } else {
            "copy into that folder"
        }));
    }
    let (db, actor, on_clash) = (c.st.db.clone(), c.agent.actor(), a.on_clash.on_clash());
    let (svol2, dvol2, dparent2, name2) =
        (svol.clone(), dvol.clone(), dparent.clone(), name.clone());
    let placed = blocking(move || {
        match svol2.stat(&src) {
            Ok(m) if m.kind != Kind::Other => {}
            Ok(_) => return Err(ToolError::denied("move or copy this kind of entry")),
            Err(e) => return Err(io_err(e)),
        }
        match dvol2.stat(&dparent2) {
            Ok(m) if m.kind == Kind::Dir => {}
            Ok(_) => {
                return Err(ToolError::new(
                    "not_a_folder",
                    "The target is not a folder.",
                ));
            }
            Err(_) => {
                return Err(ToolError::new(
                    "not_found",
                    "The target folder doesn't exist.",
                ));
            }
        }
        let now = crate::web::now();
        Ok(if is_move {
            transfer::move_entry(
                &db, &svol2, &src, &dvol2, &dparent2, &name2, on_clash, &actor, now,
            )?
        } else {
            let spec = CopySpec {
                include_hidden: false,
                shallow: false,
            };
            transfer::copy_entry(
                &db, &svol2, &src, &dvol2, &dparent2, &name2, spec, on_clash, &actor, now,
            )?
        })
    })
    .await?;
    Ok(placed_json(&from, dvol.name(), &dparent, &name, placed))
}

pub async fn delete(c: &Call, a: PathArgs, t: &mut Touched) -> ToolResult {
    let (vol, rel) = c.target(&a.path)?;
    let path = show(vol.name(), &rel);
    t.0.push(path.clone());
    if rel.is_root() {
        return Err(ToolError::new("bad_path", "A volume can't be deleted."));
    }
    let perms = c.perms(&vol, &rel);
    if !perms.contains(Perms::READ) {
        return Err(ToolError::not_found());
    }
    if !perms.contains(Perms::DELETE) {
        return Err(ToolError::denied("delete"));
    }
    let (db, actor) = (c.st.db.clone(), c.agent.actor());
    let item = blocking(move || {
        Ok(trash::delete(
            &db,
            &vol,
            &rel,
            &actor,
            trash::REASON_DELETE,
            crate::web::now(),
        )?)
    })
    .await?;
    Ok(Out::json(json!({
        "path": path,
        "trashed": true,
        "trash_id": item.id,
        "size": item.size,
        "files": item.files,
        "purge_after": trash::purge_time(&c.st.cfg, &item).map(time),
    })))
}

// ---------------------------------------------------------------------------------------
// trash

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrashArgs {
    volume: Option<String>,
}

/// Trash items this agent deleted, on volumes it may read.
fn own_item(c: &Call, item: &crate::db::TrashItem) -> bool {
    item.deleted_by == c.agent.actor() && c.can_read_volume(&item.volume)
}

pub async fn trash_list(c: &Call, a: TrashArgs) -> ToolResult {
    let db = c.st.db.clone();
    let mut items = blocking(move || Ok(db.trash_items()?)).await?;
    items.retain(|i| {
        own_item(c, i)
            && a.volume
                .as_deref()
                .is_none_or(|v| v.trim_matches('/') == i.volume)
    });
    items.sort_by_key(|i| std::cmp::Reverse(i.deleted_at));
    let list: Vec<Value> = items
        .iter()
        .map(|i| {
            let rel = RelPath::parse(&i.path).unwrap_or_default();
            json!({
                "id": i.id,
                "path": show(&i.volume, &rel),
                "type": if i.is_dir { "folder" } else { "file" },
                "size": i.size,
                "files": i.files,
                "deleted": time(i.deleted_at),
                "purge_after": trash::purge_time(&c.st.cfg, i).map(time),
            })
        })
        .collect();
    Ok(Out::json(json!({ "items": list })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdArgs {
    pub id: String,
}

pub async fn trash_restore(c: &Call, a: IdArgs, t: &mut Touched) -> ToolResult {
    t.0.push(format!("trash:{}", a.id.trim()));
    let (db, id) = (c.st.db.clone(), a.id.trim().to_string());
    let item = blocking(move || Ok(db.trash_item(&id)?))
        .await?
        .filter(|i| own_item(c, i))
        .ok_or_else(|| ToolError::new("not_found", "No such item in this agent's trash."))?;
    let rel = RelPath::parse(&item.path).map_err(|_| ToolError::failed())?;
    let vol =
        c.st.volumes
            .get(&item.volume)
            .cloned()
            .ok_or_else(|| ToolError::new("not_found", "Its volume is gone."))?;
    t.0.push(show(vol.name(), &rel));
    if !c.perms(&vol, &rel).contains(Perms::WRITE) {
        return Err(ToolError::denied("restore into that folder"));
    }
    let db = c.st.db.clone();
    let vol2 = vol.clone();
    let restored = blocking(move || Ok(trash::restore(&db, &vol2, &item)?)).await?;
    Ok(Out::json(json!({
        "restored": show(vol.name(), &restored),
        "renamed": restored != rel,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns() {
        assert!(name_matches("report", "Q3 Report.pdf"));
        assert!(name_matches("*.PDF", "q3 report.pdf"));
        assert!(!name_matches("*.pdf", "q3 report.pdf.txt"));
        assert!(name_matches("IMG_20??.jpg", "img_2041.jpg"));
        assert!(!name_matches("IMG_20??.jpg", "img_204.jpg"));
        assert!(name_matches("*a*b*", "xxaYYbzz"));
        assert!(name_matches("*", ""));
        assert!(!name_matches("a*", "ba"));
    }

    #[test]
    fn text_detection() {
        assert_eq!(text_part(b"hello", true), Some((0, 5)));
        assert_eq!(text_part(b"a\0b", true), None);
        // "ä" is C3 A4: a piece cut after C3 keeps "a" and leaves C3 for the next piece.
        assert_eq!(text_part(b"a\xC3", false), Some((0, 1)));
        assert_eq!(text_part(b"a\xC3", true), None);
        // A piece starting at A4 skips it.
        assert_eq!(text_part(b"\xA4bc", true), Some((1, 2)));
        assert_eq!(text_part(b"\xFF\xFEx", true), None);
        assert_eq!(text_part(b"", true), Some((0, 0)));
    }
}
