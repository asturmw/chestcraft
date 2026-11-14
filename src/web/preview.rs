//! Thumbnails (`?thumb`, `?thumb=view`) and the viewer page (`?view`) of a file, for
//! volumes and shares alike. The caller has checked that the visitor may read the file.
//!
//! The viewer is a page of its own, without the app shell: a top bar (close, name,
//! "3 / 18", download, share, delete, info) over a dark stage. `?view` takes the listing's
//! `sort`, `order` and `hidden` and steps through the folder's files in that order
//! (folders skipped, files without a preview included); `info=1` opens the info panel.
//! Every link keeps these parameters, and the close link returns to the listing's page
//! and row (`#row-<n>`). Everything works without scripts; `assets/viewer.js` adds zoom,
//! swipe, the delete question and the info toggle without a reload.

use super::files::{ListOrder, content_disposition, enc, listing_place};
use super::views::{self, Crumb, Ctx, ViewerBody, ViewerDelete, ViewerView};
use super::{AppState, ClientInfo};
use crate::fs::filetype::{self, Preview, ThumbSource};
use crate::fs::path::RelPath;
use crate::fs::thumbs::{self, Size, ThumbError};
use crate::fs::volume::{Entry, Kind, Meta, VolumeRoot};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use governor::{DefaultKeyedRateLimiter, Quota};
use pulldown_cmark::{CodeBlockKind, CowStr, Event, Options, Parser, Tag, TagEnd};
use std::io::Read;
use std::net::IpAddr;
use std::num::NonZeroU32;
use std::os::fd::OwnedFd;
use std::sync::Arc;

/// Text shown in the viewer at most; longer files are cut.
const TEXT_MAX: u64 = 512 << 10;

/// New thumbnails an anonymous visitor (public volume, share link) may have rendered: a
/// burst for a first folder view, then a steady trickle per IP.
pub fn anon_limiter() -> DefaultKeyedRateLimiter<IpAddr> {
    let per_sec = NonZeroU32::new(4).unwrap_or(NonZeroU32::MIN);
    let burst = NonZeroU32::new(120).unwrap_or(NonZeroU32::MIN);
    DefaultKeyedRateLimiter::keyed(Quota::per_second(per_sec).allow_burst(burst))
}

/// `?thumb` / `?thumb=view` of an open file.
#[allow(clippy::too_many_arguments)]
pub async fn thumb(
    st: &AppState,
    client: &ClientInfo,
    anonymous: bool,
    headers: &HeaderMap,
    fd: OwnedFd,
    meta: &Meta,
    name: &str,
    size: Option<&str>,
    version: Option<&str>,
) -> Response {
    let (Some(src), Some(size)) = (filetype::thumb_source(name), size.and_then(Size::parse)) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let tag = thumbs::version_tag(meta);
    let etag = format!("\"t{tag}-{}\"", size.px());
    let cache = if version == Some(tag.as_str()) {
        "private, max-age=31536000, immutable"
    } else {
        "private, no-cache"
    };
    let not_modified = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim() == etag));
    if not_modified {
        return (
            StatusCode::NOT_MODIFIED,
            [
                (header::ETAG, etag),
                (header::CACHE_CONTROL, cache.to_string()),
            ],
        )
            .into_response();
    }
    let ip = client.ip;
    let got = st
        .thumbs
        .get(fd, meta, src, size, || {
            !anonymous || st.thumb_limiter.check_key(&ip).is_ok()
        })
        .await;
    match got {
        Ok(Some(t)) => (
            [
                (header::CONTENT_TYPE, t.mime.to_string()),
                (header::ETAG, etag),
                (header::CACHE_CONTROL, cache.to_string()),
                (
                    header::CONTENT_DISPOSITION,
                    content_disposition("inline", "thumbnail"),
                ),
            ],
            t.bytes,
        )
            .into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            [(header::CACHE_CONTROL, cache.to_string())],
        )
            .into_response(),
        Err(ThumbError::Skipped(_)) => StatusCode::NOT_FOUND.into_response(),
        Err(ThumbError::Busy) => (
            StatusCode::TOO_MANY_REQUESTS,
            [(header::RETRY_AFTER, HeaderValue::from_static("5"))],
        )
            .into_response(),
        Err(e) => {
            tracing::warn!("thumbnail failed: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Query string for a thumbnail URL that changes whenever the file does.
pub fn thumb_query(meta: &Meta, size: Size) -> String {
    let which = match size {
        Size::Grid => "",
        Size::View => "=view",
    };
    format!("?thumb{which}&v={}", thumbs::version_tag(meta))
}

/// Everything the viewer page of one file needs.
pub struct ViewerReq {
    pub vol: Arc<VolumeRoot>,
    /// The file, relative to the volume.
    pub rel: RelPath,
    pub meta: Meta,
    pub fd: OwnedFd,
    /// The file's URL path (escaped, as requested).
    pub href: String,
    /// Breadcrumbs down to the folder holding the file (the info panel's location).
    pub crumbs: Vec<Crumb>,
    /// Admin with hidden files shown: they count for previous/next too.
    pub show_hidden: bool,
    /// The order of the listing the visitor came from: previous/next follow it.
    pub order: ListOrder,
    /// The info panel is open (`?info=1`).
    pub info: bool,
    /// The visitor may move the file to the trash: the CSRF token and the days the trash
    /// keeps it (0 = kept).
    pub delete: Option<(String, u32)>,
    pub share_href: Option<String>,
}

/// Where a file sits among the files of its folder, in the listing's order.
#[derive(Default)]
struct Place {
    /// Index among all entries of the folder (folders first): the listing's page and row.
    index: Option<usize>,
    /// 1-based position among the files, and how many files there are.
    position: Option<(usize, usize)>,
    prev: Option<Entry>,
    next: Option<Entry>,
}

pub async fn viewer(ctx: &Ctx<'_>, req: ViewerReq) -> Response {
    let name = req.rel.name().unwrap_or_default().to_string();
    let folder_href = match req.href.rfind('/') {
        Some(i) => req.href[..=i].to_string(),
        None => "/".into(),
    };
    // What every link out of the viewer keeps: the listing's order and hidden files.
    let mut keep = req.order.params();
    if req.show_hidden {
        keep.push("hidden=1".into());
    }
    let place = {
        let (vol, rel, hidden, order) =
            (req.vol.clone(), req.rel.clone(), req.show_hidden, req.order);
        tokio::task::spawn_blocking(move || place(&vol, &rel, hidden, order))
            .await
            .unwrap_or_default()
    };
    let view_query = |info: bool| {
        let mut q = String::from("?view");
        for p in &keep {
            q.push('&');
            q.push_str(p);
        }
        if info {
            q.push_str("&info=1");
        }
        q
    };
    let step = |e: &Entry| format!("{folder_href}{}{}", enc(&e.name), view_query(req.info));
    // Back to the folder: the page that lists the file, at its row.
    let close_href = {
        let mut params = keep.clone();
        let mut row = String::new();
        if let Some(i) = place.index {
            let (page, id) = listing_place(i);
            if page > 1 {
                params.push(format!("page={page}"));
            }
            row = format!("#{id}");
        }
        if params.is_empty() {
            format!("{folder_href}{row}")
        } else {
            format!("{folder_href}?{}{row}", params.join("&"))
        }
    };
    // The next picture, for the page script to fetch ahead.
    let next_preload = place.next.as_ref().and_then(|e| {
        matches!(
            filetype::preview(&e.name),
            Preview::Image | Preview::Rendered
        )
        .then(|| {
            format!(
                "{folder_href}{}{}",
                enc(&e.name),
                thumb_query(&e.meta, Size::View)
            )
        })
    });
    let body = body_for(&name, &req.href, &req.meta, req.fd).await;
    let dimensions = match &body {
        ViewerBody::Image {
            dims: Some((w, h)), ..
        } => Some(ctx.tf(
            "viewer.pixels",
            &[
                ("width", &ctx.number(u64::from(*w))),
                ("height", &ctx.number(u64::from(*h))),
            ],
        )),
        _ => None,
    };
    let ftype = filetype::icon_type(&name);
    let location = req
        .crumbs
        .iter()
        .map(|c| c.label.as_str())
        .collect::<Vec<_>>()
        .join(" / ");
    let view = ViewerView {
        ftype,
        type_label: ctx.t(type_key(ftype)).to_string(),
        size: ctx.size(req.meta.size),
        modified: ctx.datetime(req.meta.mtime),
        location,
        dimensions,
        download_href: format!("{}?dl", req.href),
        close_href,
        position: place.position,
        prev_href: place.prev.as_ref().map(step),
        next_href: place.next.as_ref().map(step),
        next_preload,
        delete: req.delete.map(|(csrf, trash_days)| ViewerDelete {
            action: folder_href.clone(),
            csrf,
            note: if trash_days == 0 {
                ctx.t("browser.trash_note_kept").to_string()
            } else {
                ctx.tn("browser.trash_note", u64::from(trash_days))
            },
        }),
        share_href: req.share_href,
        info_open: req.info,
        info_href: format!("{}{}", req.href, view_query(!req.info)),
        body,
        name,
    };
    views::viewer(ctx, &view).into_response()
}

/// How the file at `href` is shown: picture, player, text or nothing.
pub async fn body_for(name: &str, href: &str, meta: &Meta, fd: OwnedFd) -> ViewerBody {
    let kind = filetype::preview(name);
    let thumb = |size| format!("{href}{}", thumb_query(meta, size));
    match kind {
        Preview::Image => {
            // The size in pixels, where the file's header says it (cheap: nothing is decoded).
            let dims = if filetype::thumb_source(name) == Some(ThumbSource::Image) {
                tokio::task::spawn_blocking(move || {
                    thumbs::dimensions(std::io::BufReader::new(std::fs::File::from(fd)))
                })
                .await
                .ok()
                .flatten()
            } else {
                None
            };
            ViewerBody::Image {
                src: thumb(Size::View),
                full: href.to_string(),
                zoom_full: filetype::browser_image(name),
                dims,
            }
        }
        Preview::Rendered => ViewerBody::Rendered {
            src: thumb(Size::View),
        },
        Preview::Video => ViewerBody::Video {
            src: href.to_string(),
            poster: thumb(Size::View),
        },
        Preview::Audio => ViewerBody::Audio {
            src: href.to_string(),
        },
        Preview::Pdf => ViewerBody::Pdf {
            src: href.to_string(),
        },
        Preview::Markdown | Preview::Text | Preview::Sniff => {
            let text = tokio::task::spawn_blocking(move || read_text(fd))
                .await
                .ok()
                .flatten();
            match (kind, text) {
                (_, None) => ViewerBody::None,
                (Preview::Markdown, Some((t, cut))) => ViewerBody::Markdown {
                    html: markdown(&t),
                    truncated: cut,
                },
                (_, Some((t, cut))) => ViewerBody::Text {
                    text: t,
                    truncated: cut,
                },
            }
        }
        Preview::None => ViewerBody::None,
    }
}

pub fn type_key(ftype: &str) -> &'static str {
    match ftype {
        "folder" => "filetype.folder",
        "image" => "filetype.image",
        "video" => "filetype.video",
        "audio" => "filetype.audio",
        "pdf" => "filetype.pdf",
        "text" => "filetype.text",
        "doc" => "filetype.doc",
        "sheet" => "filetype.sheet",
        "archive" => "filetype.archive",
        "code" => "filetype.code",
        "model" => "filetype.model",
        "gcode" => "filetype.gcode",
        "vector" => "filetype.vector",
        "disk" => "filetype.disk",
        _ => "filetype.file",
    }
}

/// The start of a text file, and whether it was cut. None for binary content.
fn read_text(fd: OwnedFd) -> Option<(String, bool)> {
    let mut f = std::fs::File::from(fd);
    let mut buf = Vec::new();
    (&mut f).take(TEXT_MAX + 1).read_to_end(&mut buf).ok()?;
    let cut = buf.len() as u64 > TEXT_MAX;
    buf.truncate(TEXT_MAX as usize);
    if buf.iter().take(8192).any(|&b| b == 0) {
        return None;
    }
    // A cut may split a character; drop the broken tail.
    let text = match String::from_utf8(buf) {
        Ok(s) => s,
        Err(e) => {
            let valid = e.utf8_error().valid_up_to();
            let bytes = e.into_bytes();
            if cut && bytes.len() - valid < 4 {
                String::from_utf8_lossy(&bytes[..valid]).into_owned()
            } else {
                String::from_utf8_lossy(&bytes).into_owned()
            }
        }
    };
    Some((text, cut))
}

/// Where `rel` sits in its folder: the listing's order, folders first; previous and next
/// are its neighbours among the files (folders are skipped, files without a preview count).
fn place(vol: &VolumeRoot, rel: &RelPath, show_hidden: bool, order: ListOrder) -> Place {
    let (Some(name), Some(parent)) = (rel.name(), rel.parent()) else {
        return Place::default();
    };
    let Ok(mut entries) = vol.list(&parent, show_hidden) else {
        return Place::default();
    };
    order.sort(&mut entries);
    let index = entries.iter().position(|e| e.name == name);
    let mut files: Vec<Entry> = entries
        .into_iter()
        .filter(|e| e.meta.kind == Kind::File)
        .collect();
    let Some(i) = files.iter().position(|e| e.name == name) else {
        return Place {
            index,
            ..Place::default()
        };
    };
    let total = files.len();
    let next = (i + 1 < total).then(|| files.swap_remove(i + 1));
    let prev = (i > 0).then(|| files.swap_remove(i - 1));
    Place {
        index,
        position: Some((i + 1, total)),
        prev,
        next,
    }
}

/// Links and pictures may only point at the web or at relative paths.
fn safe_url(url: &str) -> bool {
    let u = url.trim().to_ascii_lowercase();
    if u.starts_with("http://") || u.starts_with("https://") || u.starts_with("mailto:") {
        return true;
    }
    // No scheme: a relative path, a fragment or a query.
    match u.find(':') {
        None => true,
        Some(colon) => u.find(['/', '?', '#']).is_some_and(|i| i < colon),
    }
}

fn clean(url: CowStr<'_>) -> CowStr<'_> {
    if safe_url(&url) {
        url
    } else {
        CowStr::Borrowed("#")
    }
}

/// Markdown to HTML. Raw HTML in the file is shown as code, never passed through, and
/// links with other schemes than http(s)/mailto lose their target.
pub fn markdown(src: &str) -> String {
    let opts = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES;
    let events = Parser::new_ext(src, opts).map(|ev| match ev {
        Event::Html(h) => Event::Text(h),
        Event::InlineHtml(h) => Event::Code(h),
        Event::Start(Tag::HtmlBlock) => Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(
            CowStr::Borrowed("html"),
        ))),
        Event::End(TagEnd::HtmlBlock) => Event::End(TagEnd::CodeBlock),
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Link {
            link_type,
            dest_url: clean(dest_url),
            title,
            id,
        }),
        Event::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Image {
            link_type,
            dest_url: clean(dest_url),
            title,
            id,
        }),
        other => other,
    });
    let mut out = String::new();
    pulldown_cmark::html::push_html(&mut out, events);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_never_passes_html_or_script_links() {
        let html = markdown(
            "# Hi\n\n<script>alert(1)</script>\n\nText <b onclick=\"x\">bold</b> \
             [ok](https://example.org) [rel](notes/a.md) [bad](javascript:alert(1)) \
             [bad2]( JaVaScRiPt:alert(1)) ![img](data:image/svg+xml,<svg>)\n",
        );
        assert!(!html.contains("<script"), "{html}");
        assert!(!html.contains("<b "), "{html}");
        assert!(html.contains("&lt;script&gt;"), "{html}");
        assert!(html.contains(r#"href="https://example.org""#), "{html}");
        assert!(html.contains(r#"href="notes/a.md""#), "{html}");
        assert!(!html.to_lowercase().contains("javascript:"), "{html}");
        assert!(!html.contains("data:"), "{html}");
    }

    #[test]
    fn safe_urls() {
        assert!(safe_url("https://x.org/a"));
        assert!(safe_url("../b.md"));
        assert!(safe_url("#top"));
        assert!(safe_url("a/b:c"));
        assert!(!safe_url("javascript:x"));
        assert!(!safe_url("vbscript:x"));
        assert!(!safe_url("data:text/html,x"));
    }
}
