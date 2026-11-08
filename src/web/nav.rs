//! The sidebar of the app pages (`views::Nav`): the volumes the visitor may read as a
//! folder tree, opened along the current path; the current volume's free space; for
//! accounts the number of their active shares.

use super::Shared;
use super::auth::CurrentUser;
use super::views::{Nav, Section, SpaceLine, TreeNode};
use crate::auth::perm::{Origin, Perms, Principal, volume_perms};
use crate::fs::path::RelPath;
use crate::fs::volume::VolumeRoot;
use crate::i18n::{I18n, Lang};
use std::sync::Arc;

/// Folders per level of the tree; a level with more shows the first ones.
const LEVEL_CAP: usize = 200;

/// Where the page is: a folder of a volume (`page`: the folder itself is the page; false
/// for a file in it, which the viewer shows).
pub struct Here {
    pub vol: Arc<VolumeRoot>,
    pub dir: RelPath,
    pub page: bool,
}

#[allow(clippy::too_many_arguments)]
pub async fn build(
    st: &Shared,
    lang: Lang,
    principal: &Principal,
    current: Option<&CurrentUser>,
    origin: Origin,
    section: Section,
    here: Option<Here>,
) -> Nav {
    let (st, principal, owner) = (
        st.clone(),
        principal.clone(),
        current.map(|c| c.key.clone()),
    );
    let fallback = Nav {
        section,
        tree: Vec::new(),
        space: None,
        shares: None,
    };
    tokio::task::spawn_blocking(move || {
        let i18n = &st.i18n;
        let signed_in = !principal.is_anonymous();
        let tree = st
            .volumes
            .iter()
            .filter(|v| volume_perms(&st.cfg, &principal, origin, v.name()).contains(Perms::READ))
            .map(|v| {
                let tag = if !v.cfg.remote {
                    Some(i18n.t(lang, "nav.tag_home").to_string())
                } else if v.cfg.public && signed_in {
                    Some(i18n.t(lang, "nav.tag_public").to_string())
                } else {
                    None
                };
                let href = format!("/{}/", super::files::enc(v.name()));
                let mut node = TreeNode {
                    label: v.name().to_string(),
                    href,
                    tag,
                    current: None,
                    children: Vec::new(),
                };
                if let Some(h) = here.as_ref().filter(|h| h.vol.name() == v.name()) {
                    let parts: Vec<&str> = h.dir.components().collect();
                    node.current = Some(if parts.is_empty() && h.page {
                        "page"
                    } else {
                        "location"
                    });
                    if !parts.is_empty() {
                        node.children = level(v, &RelPath::root(), &node.href, &parts, h.page);
                    }
                }
                node
            })
            .collect();
        let space = here.as_ref().and_then(|h| {
            let s = h.vol.space().ok().filter(|s| s.total > 0)?;
            Some(space_line(i18n, lang, h.vol.name(), s.avail, s.total))
        });
        let shares = owner.map(|key| {
            let now = super::now();
            st.db
                .shares()
                .unwrap_or_default()
                .iter()
                .filter(|s| {
                    s.owner == key && s.revoked.is_none() && s.expires.is_none_or(|t| t > now)
                })
                .count()
        });
        Nav {
            section,
            tree,
            space,
            shares,
        }
    })
    .await
    .unwrap_or(fallback)
}

/// The folders in `dir`; the one named `rest[0]` opens with the next level.
fn level(vol: &VolumeRoot, dir: &RelPath, href: &str, rest: &[&str], page: bool) -> Vec<TreeNode> {
    let mut names = vol.subdirs(dir, LEVEL_CAP).unwrap_or_default();
    // The way to the current folder always shows, even past the cap or when hidden.
    if !names.iter().any(|n| n == rest[0]) {
        names.push(rest[0].to_string());
    }
    names
        .into_iter()
        .map(|name| {
            let child_href = format!("{href}{}/", super::files::enc(&name));
            let mut node = TreeNode {
                label: name.clone(),
                href: child_href.clone(),
                tag: None,
                current: None,
                children: Vec::new(),
            };
            if name == rest[0] {
                let last = rest.len() == 1;
                node.current = Some(if last && page { "page" } else { "location" });
                if !last && let Ok(sub) = dir.join(&name) {
                    node.children = level(vol, &sub, &child_href, &rest[1..], page);
                }
            }
            node
        })
        .collect()
}

pub fn space_line(i18n: &I18n, lang: Lang, volume: &str, avail: u64, total: u64) -> SpaceLine {
    let used = total.saturating_sub(avail);
    SpaceLine {
        volume: volume.to_string(),
        free: i18n.tf(
            lang,
            "landing.free_of",
            &[
                ("free", &i18n.size(lang, avail)),
                ("total", &i18n.size(lang, total)),
            ],
        ),
        used_percent: (used.saturating_mul(100) / total.max(1)).min(100) as u8,
    }
}
