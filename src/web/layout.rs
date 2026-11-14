//! List or grid: how a folder listing shows its entries (accounts, public volumes, shares).
//!
//! In this order: `?layout=list|grid` in the address (the toggle's links); what the person
//! chose last (accounts: `UserSettings.layout`, with "automatic" on the account page;
//! visitors: the `cc_layout` cookie of that browser); else by content: a folder of
//! pictures and videos opens as a grid, anything else as a list.
//!
//! The toggle is a plain link, so it works without a script: a request that names a
//! layout is the choice, and the answer remembers it (no redirect, so the address keeps
//! working where cookies are off).

use super::query::Query;
use super::{ClientInfo, CurrentUser, Shared, cookie};
use crate::fs::filetype;
use crate::fs::volume::{Entry, Kind};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::Response;

/// Holds a visitor's choice for a year (a preference, nothing secret).
const COOKIE: &str = "cc_layout";
/// Fewer entries never flip to the grid by themselves.
const AUTO_MIN_ENTRIES: usize = 4;
/// Share of pictures and videos among the entries from which a folder opens as a grid.
const AUTO_MEDIA_PERCENT: usize = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    List,
    Grid,
}

impl Layout {
    pub fn parse(s: &str) -> Option<Layout> {
        match s {
            "list" => Some(Layout::List),
            "grid" => Some(Layout::Grid),
            _ => None,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Layout::List => "list",
            Layout::Grid => "grid",
        }
    }

    /// By content: a grid when at least [`AUTO_MEDIA_PERCENT`] % of the entries shown (one
    /// page of a long folder) are pictures or videos, by name. Folders count as entries.
    pub fn by_content(entries: &[Entry]) -> Layout {
        let media = entries
            .iter()
            .filter(|e| {
                e.meta.kind != Kind::Dir
                    && matches!(filetype::icon_type(&e.name), "image" | "video")
            })
            .count();
        if entries.len() >= AUTO_MIN_ENTRIES && media * 100 >= entries.len() * AUTO_MEDIA_PERCENT {
            Layout::Grid
        } else {
            Layout::List
        }
    }
}

/// What a request says about the layout.
#[derive(Debug, Clone, Copy, Default)]
pub struct Choice {
    /// `?layout=` of this request. Links of the page keep it.
    pub explicit: Option<Layout>,
    /// The account's setting, or the visitor's cookie.
    stored: Option<Layout>,
}

impl Choice {
    /// `current`: the account whose setting counts; None reads the cookie (visitors, and
    /// everyone on share pages).
    pub fn of(q: &Query, headers: &HeaderMap, current: Option<&CurrentUser>) -> Choice {
        let stored = match current {
            Some(cu) => cu.settings.layout.as_deref(),
            None => cookie(headers, COOKIE),
        };
        Choice {
            explicit: q.get("layout").and_then(Layout::parse),
            stored: stored.and_then(Layout::parse),
        }
    }

    /// The person's choice; None leaves it to the content.
    pub fn chosen(&self) -> Option<Layout> {
        self.explicit.or(self.stored)
    }

    /// After the listing rendered: keep a layout the request asked for, in the account or
    /// as a cookie on `res`.
    pub async fn remember(
        &self,
        st: &Shared,
        client: &ClientInfo,
        current: Option<&CurrentUser>,
        res: &mut Response,
    ) {
        let Some(layout) = self.explicit else { return };
        if self.stored == Some(layout) || !res.status().is_success() {
            return;
        }
        match current {
            Some(cu) => {
                let (st, key) = (st.clone(), cu.key.clone());
                let saved = tokio::task::spawn_blocking(move || {
                    st.db.update_user(&key, |u| {
                        u.settings.layout = Some(layout.code().to_string());
                    })
                })
                .await;
                if !matches!(saved, Ok(Ok(_))) {
                    tracing::warn!("the layout choice was not saved");
                }
            }
            None => {
                let secure = if client.https { "; Secure" } else { "" };
                let value = format!(
                    "{COOKIE}={}; Path=/; SameSite=Lax; Max-Age=31536000{secure}",
                    layout.code()
                );
                if let Ok(v) = HeaderValue::from_str(&value) {
                    res.headers_mut().append(header::SET_COOKIE, v);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::volume::Meta;

    fn entries(names: &[&str]) -> Vec<Entry> {
        names
            .iter()
            .map(|n| Entry {
                name: n.trim_end_matches('/').to_string(),
                meta: Meta {
                    kind: if n.ends_with('/') {
                        Kind::Dir
                    } else {
                        Kind::File
                    },
                    size: 0,
                    mtime: 0,
                    mtime_nsec: 0,
                    ino: 0,
                    dev: 0,
                },
            })
            .collect()
    }

    #[test]
    fn grid_from_four_entries_and_eighty_percent_media() {
        let grid = |names: &[&str]| Layout::by_content(&entries(names)) == Layout::Grid;
        assert!(grid(&["a.jpg", "b.PNG", "c.heic", "d.mp4"]));
        assert!(!grid(&["a.jpg", "b.png", "c.heic"]), "too few");
        assert!(grid(&["a.jpg", "b.jpg", "c.jpg", "d.mov", "notes.txt"]));
        assert!(!grid(&["a.jpg", "b.jpg", "c.jpg", "notes.txt"]), "75 %");
        // A folder is an entry that is not a picture, whatever its name.
        assert!(!grid(&["a.jpg", "b.jpg", "c.jpg", "raw.jpg/"]));
        assert!(grid(&["a.jpg", "b.jpg", "c.jpg", "d.jpg", "raw/"]));
        assert!(!grid(&[]));
    }
}
