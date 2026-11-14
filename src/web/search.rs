//! Search by name for accounts (review Q12, Q13, Q37):
//!
//! ```text
//! GET /.cc/search?q=<words>[&vol=<library>][&hidden=1][&from=tab]
//!   q         the words; each must occur in the name of a file or folder, in any order,
//!             without case or accents (fs::search). Empty: the page with the field, no walk.
//!   vol       the library the search started from (the app bar's field on a folder page,
//!             the phone's Search tab): searched first, the others follow in sidebar order
//!   hidden=1  admins: dot-names too
//!   from=tab  the phone's Search tab: an empty field takes the focus
//!   200 text/html, streamed
//! ```
//!
//! Every library the account may read from the request's origin is walked with the tree's
//! rules (`fs::tree::walk`: no symlinks, no internal names, dot-names only for admins with
//! `hidden=1`). Names only, no contents, no index. Visitors (shares, public volumes) have no
//! search; anonymous requests go to the sign-in page.
//!
//! The answer is one HTML page that streams, so it works without script and shows hits as
//! the walk finds them: the shell, the field and the table head go out at once, rows follow
//! (sent at most every 100 ms), then the count and the notices, then the rest of the shell.
//! The rows come in walk order: the first library, then by path in natural order.
//!
//! Bounds: at most [`MAX_HITS`] rows (then "refine"), [`MAX_TIME`] from the request
//! (waiting included), [`MAX_ENTRIES`] entries walked; at most [`PARALLEL`] walks at a
//! time across all accounts. One search per account: a new one stops the one still
//! running (its page ends with a note), so a burst of Enter can't pile up walks.

use super::nav;
use super::query::Query;
use super::search_views::{self, End, Hit, Outcome, SearchView};
use super::views::Section;
use super::{ClientInfo, Current, Shared, page_ctx};
use crate::auth::perm::{Perms, Principal, volume_perms};
use crate::fs::filetype;
use crate::fs::path::RelPath;
use crate::fs::search::Matcher;
use crate::fs::thumbs::Size;
use crate::fs::tree::{self, Found, Step};
use crate::fs::volume::VolumeRoot;
use axum::body::Body;
use axum::extract::{Extension, State};
use axum::http::{HeaderMap, HeaderValue, Uri, header};
use axum::response::{IntoResponse, Redirect, Response};
use bytes::Bytes;
use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{Semaphore, mpsc};

pub const MAX_HITS: usize = 500;
pub const MAX_TIME: Duration = Duration::from_secs(10);
pub const MAX_ENTRIES: u64 = 2_000_000;
pub const PARALLEL: usize = 2;
/// Rows wait at most this long before they are sent.
const FLUSH_EVERY: Duration = Duration::from_millis(100);

/// The running searches: the walk slots, and per account the one search it may run.
pub struct Searches {
    walks: Arc<Semaphore>,
    running: Mutex<HashMap<String, (u64, Arc<AtomicBool>)>>,
    next: AtomicU64,
}

impl Default for Searches {
    fn default() -> Self {
        Searches {
            walks: Arc::new(Semaphore::new(PARALLEL)),
            running: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
        }
    }
}

/// One account's running search; dropping it frees the account's place.
pub struct Ticket {
    st: Shared,
    account: String,
    id: u64,
    stop: Arc<AtomicBool>,
}

impl Ticket {
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        let mut running = self
            .st
            .searches
            .running
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if running
            .get(&self.account)
            .is_some_and(|(id, _)| *id == self.id)
        {
            running.remove(&self.account);
        }
    }
}

/// Register a new search of `account`, stopping the one it still has running.
fn start(st: &Shared, account: &str) -> Ticket {
    let id = st.searches.next.fetch_add(1, Ordering::Relaxed);
    let stop = Arc::new(AtomicBool::new(false));
    let mut running = st
        .searches
        .running
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some((_, old)) = running.insert(account.to_string(), (id, stop.clone())) {
        old.store(true, Ordering::Relaxed);
    }
    Ticket {
        st: st.clone(),
        account: account.to_string(),
        id,
        stop,
    }
}

pub async fn page(
    State(st): State<Shared>,
    Extension(client): Extension<ClientInfo>,
    Extension(principal): Extension<Principal>,
    Extension(current): Extension<Current>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let Some(cu) = current.clone() else {
        return Redirect::to(&format!(
            "/.cc/login?next={}",
            super::files::enc(&uri.to_string())
        ))
        .into_response();
    };
    let started = Instant::now();
    let q = Query::parse(uri.query());
    let query = q.get("q").unwrap_or("").to_string();
    let hidden = principal.is_admin() && q.get("hidden") == Some("1");
    // Readable libraries in sidebar order, the one the search started from first.
    let mut vols: Vec<Arc<VolumeRoot>> = st
        .volumes
        .iter()
        .filter(|v| {
            volume_perms(&st.cfg, &principal, client.origin, v.name()).contains(Perms::READ)
        })
        .cloned()
        .collect();
    let first = q
        .get("vol")
        .and_then(|name| vols.iter().position(|v| v.name() == name));
    if let Some(i) = first {
        let v = vols.remove(i);
        vols.insert(0, v);
    }
    let matcher = Matcher::new(&query);

    let ctx = page_ctx(&st, &headers, &principal, current.as_deref());
    let nav = nav::build(
        &st,
        ctx.lang,
        &principal,
        current.as_deref(),
        client.origin,
        Section::Search,
        None,
    )
    .await;
    let ctx = ctx.with_nav(nav);
    let view = SearchView {
        autofocus: q.get("from") == Some("tab") && matcher.is_none(),
        scope: match vols.as_slice() {
            [one] => ctx.tf("search.scope_one", &[("volume", one.name())]),
            _ => ctx.t("search.scope_all").to_string(),
        },
        searching: matcher.is_some(),
        vol: first.map(|_| vols[0].name().to_string()),
        admin: principal.is_admin(),
        hidden,
        query,
    };
    let page = search_views::page(&ctx, &view).into_string();
    let html = |body: Body| {
        let mut res = Response::new(body);
        let h = res.headers_mut();
        h.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        );
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        res
    };
    let Some(matcher) = matcher else {
        return html(Body::from(page));
    };
    let Some((head, middle, end)) = page
        .split_once(search_views::ROWS)
        .and_then(|(head, rest)| {
            let (middle, end) = rest.split_once(search_views::TAIL)?;
            Some((head.to_string(), middle.to_string(), end.to_string()))
        })
    else {
        return html(Body::from(page));
    };

    let (tx, rx) = mpsc::channel::<io::Result<Bytes>>(16);
    let _ = tx.try_send(Ok(Bytes::from(head)));
    let ticket = start(&st, &cu.key);
    let deadline = started + MAX_TIME;
    let st2 = st.clone();
    tokio::spawn(async move {
        let permit =
            tokio::time::timeout_at(deadline.into(), st2.searches.walks.clone().acquire_owned())
                .await;
        let _ = tokio::task::spawn_blocking(move || {
            let ctx = page_ctx(&st2, &headers, &principal, current.as_deref());
            let mut out = Sink::new(tx);
            let outcome = match permit {
                Ok(Ok(_permit)) if !ticket.stopped() => {
                    let walk = Walk {
                        ctx: &ctx,
                        matcher: &matcher,
                        hidden,
                        ticket: &ticket,
                        deadline,
                    };
                    let o = walk.run(&vols, &mut out);
                    tracing::debug!(
                        hits = o.hits,
                        end = ?o.end,
                        ms = started.elapsed().as_millis() as u64,
                        "search"
                    );
                    o
                }
                Ok(Ok(_)) => Outcome {
                    hits: 0,
                    end: End::Replaced,
                    failed: Vec::new(),
                },
                _ => Outcome {
                    hits: 0,
                    end: End::Busy,
                    failed: Vec::new(),
                },
            };
            drop(ticket);
            out.push(&middle);
            out.push(
                &search_views::tail(&ctx, &view, &outcome, MAX_HITS, MAX_TIME.as_secs())
                    .into_string(),
            );
            out.push(&end);
            out.flush();
        })
        .await;
    });
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    html(Body::from_stream(stream))
}

/// Collects markup and sends it in pieces: when asked, or when rows have waited long
/// enough.
struct Sink {
    tx: mpsc::Sender<io::Result<Bytes>>,
    buf: String,
    sent: Instant,
    gone: bool,
}

impl Sink {
    fn new(tx: mpsc::Sender<io::Result<Bytes>>) -> Sink {
        Sink {
            tx,
            buf: String::new(),
            sent: Instant::now(),
            gone: false,
        }
    }

    fn push(&mut self, s: &str) {
        self.buf.push_str(s);
    }

    /// Send what waits. False once the visitor has gone.
    fn flush(&mut self) -> bool {
        if !self.buf.is_empty() && !self.gone {
            let chunk = Bytes::from(std::mem::take(&mut self.buf));
            self.gone = self.tx.blocking_send(Ok(chunk)).is_err();
            self.sent = Instant::now();
        }
        !self.gone
    }

    /// Send rows that have waited long enough. False once the visitor has gone.
    fn tick(&mut self) -> bool {
        if !self.buf.is_empty() && self.sent.elapsed() >= FLUSH_EVERY {
            return self.flush();
        }
        !self.gone && !self.tx.is_closed()
    }
}

struct Walk<'a, 'c> {
    ctx: &'a super::views::Ctx<'c>,
    matcher: &'a Matcher,
    hidden: bool,
    ticket: &'a Ticket,
    deadline: Instant,
}

impl Walk<'_, '_> {
    fn run(&self, vols: &[Arc<VolumeRoot>], out: &mut Sink) -> Outcome {
        let mut o = Outcome {
            hits: 0,
            end: End::Done,
            failed: Vec::new(),
        };
        let mut seen: u64 = 0;
        let mut gone = false;
        for vol in vols {
            let walked = tree::walk(vol, &RelPath::root(), self.hidden, None, &mut |f| {
                seen += 1;
                if seen >= MAX_ENTRIES {
                    o.end = End::Entries;
                    return Step::Stop;
                }
                if seen.is_multiple_of(64) {
                    if self.ticket.stopped() {
                        o.end = End::Replaced;
                        return Step::Stop;
                    }
                    if Instant::now() >= self.deadline {
                        o.end = End::Time;
                        return Step::Stop;
                    }
                }
                if self.matcher.matches(f.name)
                    && let Some(hit) = self.hit(vol, f)
                {
                    if o.hits == MAX_HITS {
                        o.end = End::Full;
                        return Step::Stop;
                    }
                    o.hits += 1;
                    out.push(&search_views::row(&hit).into_string());
                }
                if !out.tick() {
                    gone = true;
                    return Step::Stop;
                }
                Step::Go
            });
            match walked {
                Ok(true) => {}
                Ok(false) => break,
                Err(e) => {
                    tracing::warn!(volume = %vol.name(), "search: {e}");
                    o.failed.push(vol.name().to_string());
                }
            }
        }
        if gone {
            tracing::debug!("search: the visitor left");
        }
        o
    }

    fn hit(&self, vol: &VolumeRoot, f: &Found) -> Option<Hit> {
        let ctx = self.ctx;
        let rel = RelPath::parse(f.path).ok()?;
        let parent = rel.parent().unwrap_or_default();
        let root = format!("/{}/", super::files::enc(vol.name()));
        let url = format!("{root}{}", rel.to_url());
        let location_href = if parent.is_root() {
            root.clone()
        } else {
            format!("{root}{}/", parent.to_url())
        };
        let location = std::iter::once(vol.name())
            .chain(parent.components())
            .collect::<Vec<_>>()
            .join(" / ");
        let date = ctx.date(f.meta.mtime);
        let (href, size, meta, ftype, thumb) = if f.dir {
            (format!("{url}/"), String::new(), date, "folder", None)
        } else {
            let size = ctx.size(f.meta.size);
            let keep = if self.hidden { "&hidden=1" } else { "" };
            (
                format!("{url}?view{keep}"),
                size.clone(),
                ctx.tf("browser.meta", &[("size", &size), ("date", &date)]),
                filetype::icon_type(f.name),
                filetype::thumb_source(f.name)
                    .map(|_| format!("{url}{}", super::preview::thumb_query(&f.meta, Size::Grid))),
            )
        };
        Some(Hit {
            name: f.name.to_string(),
            ftype,
            href,
            thumb,
            location,
            location_href,
            size,
            meta,
            modified: ctx.datetime(f.meta.mtime),
            modified_iso: ctx.iso_local(f.meta.mtime),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_search_stops_the_accounts_running_one() {
        let (st, _tmp) = super::super::tests::state_for_tests();
        let a1 = start(&st, "anna");
        let b = start(&st, "ben");
        assert!(!a1.stopped() && !b.stopped());
        let a2 = start(&st, "anna");
        assert!(a1.stopped(), "the older search of the same account stops");
        assert!(!a2.stopped() && !b.stopped());
        // The old one ending doesn't take the new one's place.
        drop(a1);
        assert!(st.searches.running.lock().unwrap().contains_key("anna"));
        drop(a2);
        assert!(!st.searches.running.lock().unwrap().contains_key("anna"));
    }

    #[test]
    fn a_walk_ends_at_its_deadline_or_when_replaced() {
        let (st, tmp) = super::super::tests::state_for_tests();
        for i in 0..200 {
            std::fs::write(tmp.path().join(format!("medien/datei_{i:03}.txt")), b"x").unwrap();
        }
        let admin = Principal::Admin { name: "a".into() };
        let ctx = page_ctx(&st, &HeaderMap::new(), &admin, None);
        let matcher = Matcher::new("datei").unwrap();
        let vols: Vec<_> = st.volumes.get("Medien").cloned().into_iter().collect();
        let (tx, _rx) = mpsc::channel(1024);
        let mut out = Sink::new(tx);
        let ticket = start(&st, "anna");
        let walk = |deadline| Walk {
            ctx: &ctx,
            matcher: &matcher,
            hidden: false,
            ticket: &ticket,
            deadline,
        };
        let later = Instant::now() + Duration::from_secs(60);
        let o = walk(later).run(&vols, &mut out);
        assert_eq!((o.end, o.hits), (End::Done, 200));
        // Out of time: checked every 64 entries.
        let o = walk(Instant::now()).run(&vols, &mut out);
        assert_eq!(o.end, End::Time);
        assert!(o.hits < 200, "{}", o.hits);
        // A newer search of the same account.
        let _newer = start(&st, "anna");
        let o = walk(later).run(&vols, &mut out);
        assert_eq!(o.end, End::Replaced);
    }
}
