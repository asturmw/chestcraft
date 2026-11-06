//! Appearance: skins, backgrounds and their rotation.
//!
//! The admin sets the default skin, the skin of pages for visitors without an account, a
//! default background per skin, the visitors' background and the default rotation mode
//! (stored in the database, see [`crate::db::Appearance`]). Each account may override its
//! skin, its background per skin and the rotation mode ([`crate::db::UserSettings`]).
//!
//! A background is nothing (the skin's backdrop color), a color or gradient, or one or more
//! image sets. An image set holds up to three pictures (landscape 16:9, near-square 3:4,
//! tall 10:16); the page picks one by the screen's aspect ratio, and a missing one falls
//! back to the closest that exists. Several sets rotate: never (the first), on every page
//! load, once per session, or once per day.

pub mod images;

use crate::db::{Appearance, Background, BackgroundSet, Db, Fill};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

pub use images::{AddError, MAX_UPLOAD_BYTES};

/// Picture slots, widest first.
pub const SLOTS: [&str; 3] = ["wide", "square", "tall"];
pub const ROTATIONS: [&str; 4] = ["never", "load", "session", "day"];
pub const DEFAULT_ROTATION: &str = "day";
/// Highest dim and blur step.
pub const MAX_STEP: u8 = 4;
/// Dim step of a new image background (keeps text readable over busy pictures).
pub const DEFAULT_DIM: u8 = 2;
/// Name used for the skin when the build embeds none.
pub const FALLBACK_SKIN: &str = "default";
/// Colors in a gradient.
pub const MAX_COLORS: usize = 3;

/// Slots to try for `slot`, closest aspect ratio first.
pub fn fallback_order(slot: &str) -> [&'static str; 3] {
    match slot {
        "wide" => ["wide", "square", "tall"],
        "square" => ["square", "tall", "wide"],
        _ => ["tall", "square", "wide"],
    }
}

/// The slot file a screen of `slot`'s shape gets from `set`.
pub fn slot_for(set: &BackgroundSet, slot: &str) -> Option<&'static str> {
    fallback_order(slot)
        .into_iter()
        .find(|s| set.images.iter().any(|i| i.slot == *s))
}

pub fn is_slot(s: &str) -> bool {
    SLOTS.contains(&s)
}

pub fn is_rotation(s: &str) -> bool {
    ROTATIONS.contains(&s)
}

/// The skin to use: `wanted` if this build has it, else `fallback`, else the first skin.
pub fn pick_skin(skins: &[String], wanted: Option<&str>, fallback: Option<&str>) -> String {
    [wanted, fallback]
        .into_iter()
        .flatten()
        .find(|s| skins.iter().any(|k| k == s))
        .map(str::to_string)
        .or_else(|| skins.first().cloned())
        .unwrap_or_else(|| FALLBACK_SKIN.to_string())
}

/// `#rrggbb` (any case, `#` optional) → lowercase `#rrggbb`.
pub fn parse_color(s: &str) -> Option<String> {
    let hex = s.trim().strip_prefix('#').unwrap_or(s.trim());
    (hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| format!("#{}", hex.to_ascii_lowercase()))
}

/// A color fill with checked values, or None.
pub fn color_fill(colors: &[&str], angle: u16) -> Option<Fill> {
    let colors: Vec<String> = colors
        .iter()
        .map(|c| parse_color(c))
        .collect::<Option<_>>()?;
    (1..=MAX_COLORS)
        .contains(&colors.len())
        .then_some(Fill::Color {
            colors,
            angle: angle % 360,
        })
}

/// Query string of the stylesheet for a color fill: `c=1a2b3c-445566&a=135`.
pub fn color_query(colors: &[String], angle: u16) -> String {
    let c: Vec<&str> = colors.iter().map(|c| c.trim_start_matches('#')).collect();
    format!("c={}&a={}", c.join("-"), angle % 360)
}

/// The stylesheet `/.cc/backdrop.css?c=…&a=…` serves: it sets the backdrop color token.
/// None when the values are not valid colors.
pub fn color_css(c: &str, a: &str) -> Option<String> {
    let colors: Vec<String> = c.split('-').map(parse_color).collect::<Option<_>>()?;
    if !(1..=MAX_COLORS).contains(&colors.len()) {
        return None;
    }
    let angle: u16 = if a.is_empty() { 180 } else { a.parse().ok()? };
    let value = if colors.len() == 1 {
        colors[0].clone()
    } else {
        format!("linear-gradient({}deg, {})", angle % 360, colors.join(", "))
    };
    Some(format!(
        ".backdrop[data-state=\"color\"] {{ --backdrop-color: {value}; }}\n"
    ))
}

/// What decides the pick among several image sets.
#[derive(Debug, Clone, Copy)]
pub enum Seed<'a> {
    /// A signed-in browser (its session id hash).
    Session(&'a str),
    /// No session: "per session" rotates per day.
    Anonymous,
}

/// Index of the set to show among `n` sets. `day` counts days in the server's time zone.
pub fn pick(n: usize, mode: &str, seed: Seed, day: i64) -> usize {
    if n <= 1 {
        return 0;
    }
    match (mode, seed) {
        ("never", _) => 0,
        ("load", _) => crate::auth::random::index(n),
        ("session", Seed::Session(id)) => (fnv1a(id.as_bytes()) % n as u64) as usize,
        _ => day.rem_euclid(n as i64) as usize,
    }
}

fn fnv1a(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// Days since 1970 in `tz`.
pub fn day_number(now: jiff::Timestamp, tz: &jiff::tz::TimeZone) -> i64 {
    let offset = i64::from(tz.to_offset(now).seconds());
    (now.as_second() + offset).div_euclid(86_400)
}

/// A background with every value inside its range.
pub fn clamp(mut bg: Background) -> Background {
    bg.dim = bg.dim.min(MAX_STEP);
    bg.blur = bg.blur.min(MAX_STEP);
    bg
}

/// Ids of the image sets a background refers to.
pub fn set_ids(bg: &Background) -> &[String] {
    match &bg.fill {
        Fill::Images { sets } => sets,
        _ => &[],
    }
}

/// Every set the appearance settings show (those are visible to everyone).
fn appearance_sets(a: &Appearance) -> impl Iterator<Item = &String> {
    a.backgrounds
        .values()
        .chain(a.share_background.as_ref())
        .flat_map(set_ids)
}

/// Remove `id` from a background; an image background left without sets becomes None.
fn without_set(bg: &Background, id: &str) -> Option<Background> {
    match &bg.fill {
        Fill::Images { sets } if sets.iter().any(|s| s == id) => {
            let sets: Vec<String> = sets.iter().filter(|s| *s != id).cloned().collect();
            (!sets.is_empty()).then(|| Background {
                fill: Fill::Images { sets },
                ..bg.clone()
            })
        }
        _ => Some(bg.clone()),
    }
}

/// In-memory copy of the appearance settings and the background sets (pages read them on
/// every request), backed by the database and `<state_dir>/backgrounds`.
pub struct Store {
    dir: PathBuf,
    appearance: RwLock<Arc<Appearance>>,
    sets: RwLock<BTreeMap<String, BackgroundSet>>,
    /// One picture conversion at a time (decoding a large photo takes memory and seconds).
    work: tokio::sync::Semaphore,
}

impl Store {
    /// Load from the database (blocking). Problems are logged; the store starts empty.
    pub fn open(db: &Db, state_dir: &Path) -> Store {
        let appearance = db.appearance().unwrap_or_else(|e| {
            tracing::error!("appearance settings: {e}");
            Appearance::default()
        });
        let sets = db
            .background_sets()
            .unwrap_or_else(|e| {
                tracing::error!("background sets: {e}");
                Vec::new()
            })
            .into_iter()
            .map(|s| (s.id.clone(), s))
            .collect();
        Store {
            dir: state_dir.join("backgrounds"),
            appearance: RwLock::new(Arc::new(appearance)),
            sets: RwLock::new(sets),
            work: tokio::sync::Semaphore::new(1),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn appearance(&self) -> Arc<Appearance> {
        self.appearance.read().unwrap().clone()
    }

    /// Save new appearance settings (blocking).
    pub fn set_appearance(&self, db: &Db, a: Appearance) -> crate::db::Result<()> {
        db.put_appearance(&a)?;
        *self.appearance.write().unwrap() = Arc::new(a);
        Ok(())
    }

    pub fn set(&self, id: &str) -> Option<BackgroundSet> {
        self.sets.read().unwrap().get(id).cloned()
    }

    /// Sets of one account, oldest first.
    pub fn sets_of(&self, owner: &str) -> Vec<BackgroundSet> {
        let mut v: Vec<BackgroundSet> = self
            .sets
            .read()
            .unwrap()
            .values()
            .filter(|s| s.owner == owner)
            .cloned()
            .collect();
        v.sort_by(|a, b| a.created_ms.cmp(&b.created_ms).then(a.id.cmp(&b.id)));
        v
    }

    /// Shown by the appearance settings, so anyone may load it.
    pub fn is_public(&self, id: &str) -> bool {
        appearance_sets(&self.appearance()).any(|s| s == id)
    }

    /// File of one picture: `<id>/<slot>.jpg` or `<id>/<slot>.small.jpg`.
    pub fn file(&self, id: &str, slot: &str, small: bool) -> PathBuf {
        let name = if small {
            format!("{slot}.small.jpg")
        } else {
            format!("{slot}.jpg")
        };
        self.dir.join(id).join(name)
    }

    /// Convert and store a new set (async: waits for its turn, converts on a blocking
    /// thread). `pictures`: slot → uploaded bytes.
    pub async fn add(
        self: &Arc<Self>,
        db: Arc<Db>,
        owner: &str,
        pictures: Vec<(String, Vec<u8>)>,
        now_ms: i64,
    ) -> Result<BackgroundSet, AddError> {
        let _turn = self.work.acquire().await.map_err(|_| AddError::Busy)?;
        let (store, owner) = (self.clone(), owner.to_string());
        tokio::task::spawn_blocking(move || {
            let set = images::write_set(&store.dir, &owner, pictures, now_ms)?;
            if let Err(e) = db.put_background_set(&set) {
                let _ = std::fs::remove_dir_all(store.dir.join(&set.id));
                return Err(AddError::Storage(e.to_string()));
            }
            store
                .sets
                .write()
                .unwrap()
                .insert(set.id.clone(), set.clone());
            Ok(set)
        })
        .await
        .map_err(|_| AddError::Storage("conversion crashed".into()))?
    }

    /// Remove a set (blocking): its record, its files, and every place the appearance
    /// settings or its owner's settings use it.
    pub fn remove(&self, db: &Db, id: &str) -> crate::db::Result<()> {
        let Some(set) = self.set(id) else {
            return Ok(());
        };
        let a = self.appearance();
        if appearance_sets(&a).any(|s| s == id) {
            let mut a = (*a).clone();
            a.backgrounds = a
                .backgrounds
                .iter()
                .filter_map(|(skin, bg)| Some((skin.clone(), without_set(bg, id)?)))
                .collect();
            a.share_background = a.share_background.and_then(|bg| without_set(&bg, id));
            self.set_appearance(db, a)?;
        }
        db.update_user(&set.owner, |u| {
            u.settings.backgrounds = u
                .settings
                .backgrounds
                .iter()
                .filter_map(|(skin, bg)| Some((skin.clone(), without_set(bg, id)?)))
                .collect();
        })?;
        db.delete_background_set(id)?;
        self.sets.write().unwrap().remove(id);
        if let Err(e) = std::fs::remove_dir_all(self.dir.join(id))
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(set = id, "removing background files failed: {e}");
        }
        Ok(())
    }

    /// At startup (blocking): remove half-written sets and folders without a record, and
    /// forget records whose folder is gone.
    pub fn sweep(&self, db: &Db) -> usize {
        let mut removed = 0;
        if let Ok(entries) = std::fs::read_dir(&self.dir) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                let known = self.sets.read().unwrap().contains_key(&name);
                if !known && e.file_type().is_ok_and(|t| t.is_dir()) {
                    match std::fs::remove_dir_all(e.path()) {
                        Ok(()) => removed += 1,
                        Err(err) => tracing::warn!("removing {name} failed: {err}"),
                    }
                }
            }
        }
        let gone: Vec<String> = self
            .sets
            .read()
            .unwrap()
            .keys()
            .filter(|id| !self.dir.join(id).is_dir())
            .cloned()
            .collect();
        for id in gone {
            tracing::warn!(set = id, "background files are missing; dropping the set");
            if self.remove(db, &id).is_ok() {
                removed += 1;
            }
        }
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::BackgroundImage;

    fn set(slots: &[&str]) -> BackgroundSet {
        BackgroundSet {
            id: "x".into(),
            owner: "u".into(),
            created_ms: 0,
            images: slots
                .iter()
                .map(|s| BackgroundImage {
                    slot: s.to_string(),
                    width: 1,
                    height: 1,
                    bytes: 1,
                })
                .collect(),
        }
    }

    #[test]
    fn missing_slots_fall_back_to_the_closest_shape() {
        let only_wide = set(&["wide"]);
        assert_eq!(slot_for(&only_wide, "tall"), Some("wide"));
        let wide_tall = set(&["wide", "tall"]);
        assert_eq!(slot_for(&wide_tall, "square"), Some("tall"));
        assert_eq!(slot_for(&wide_tall, "wide"), Some("wide"));
        let square = set(&["square"]);
        assert_eq!(slot_for(&square, "wide"), Some("square"));
        assert_eq!(slot_for(&set(&[]), "wide"), None);
    }

    #[test]
    fn skins_fall_back_in_order() {
        let skins = vec!["classic".to_string(), "sleek".to_string()];
        assert_eq!(pick_skin(&skins, Some("sleek"), Some("classic")), "sleek");
        assert_eq!(pick_skin(&skins, Some("gone"), Some("sleek")), "sleek");
        assert_eq!(pick_skin(&skins, None, Some("gone")), "classic");
        assert_eq!(pick_skin(&[], Some("sleek"), None), FALLBACK_SKIN);
    }

    #[test]
    fn colors_are_checked_and_become_a_stylesheet() {
        assert_eq!(parse_color("#1A2b3C").as_deref(), Some("#1a2b3c"));
        assert_eq!(parse_color("1a2b3c").as_deref(), Some("#1a2b3c"));
        for bad in ["#12345", "#1234567", "red", "#12345g", "", "#1a2b3c;}"] {
            assert_eq!(parse_color(bad), None, "{bad}");
        }
        assert!(color_fill(&[], 0).is_none());
        assert!(color_fill(&["#000000"; 4], 0).is_none());
        assert_eq!(
            color_fill(&["#AABBCC", "#001122"], 405),
            Some(Fill::Color {
                colors: vec!["#aabbcc".into(), "#001122".into()],
                angle: 45
            })
        );
        let q = color_query(&["#aabbcc".into(), "#001122".into()], 45);
        assert_eq!(q, "c=aabbcc-001122&a=45");
        assert_eq!(
            color_css("aabbcc-001122", "45").unwrap(),
            ".backdrop[data-state=\"color\"] { --backdrop-color: linear-gradient(45deg, #aabbcc, #001122); }\n"
        );
        assert!(color_css("aabbcc", "").unwrap().contains("#aabbcc;"));
        for (c, a) in [
            ("aabbcc}", "0"),
            ("", "0"),
            ("aabbcc", "x"),
            ("a-b-c-d", "0"),
        ] {
            assert_eq!(color_css(c, a), None, "{c} {a}");
        }
    }

    #[test]
    fn rotation_picks() {
        assert_eq!(pick(1, "load", Seed::Anonymous, 5), 0);
        assert_eq!(pick(3, "never", Seed::Anonymous, 5), 0);
        assert_eq!(pick(3, "day", Seed::Anonymous, 5), 2);
        assert_eq!(pick(3, "day", Seed::Anonymous, 6), 0);
        // A session keeps its pick; without one, "per session" goes by the day.
        let a = pick(5, "session", Seed::Session("abc"), 1);
        assert!((0..20).all(|d| pick(5, "session", Seed::Session("abc"), d) == a));
        assert_eq!(pick(3, "session", Seed::Anonymous, 7), 1);
        assert!((0..50).all(|_| pick(4, "load", Seed::Anonymous, 0) < 4));
        let ts: jiff::Timestamp = "2026-11-05T23:30:00Z".parse().unwrap();
        let berlin = jiff::tz::TimeZone::get("Europe/Berlin").unwrap();
        // 00:30 on the 6th in Berlin, still the 5th in UTC.
        assert_eq!(
            day_number(ts, &berlin),
            day_number(ts, &jiff::tz::TimeZone::UTC) + 1
        );
    }

    #[test]
    fn removing_a_set_drops_empty_image_backgrounds() {
        let bg = |sets: &[&str]| Background {
            fill: Fill::Images {
                sets: sets.iter().map(|s| s.to_string()).collect(),
            },
            dim: 1,
            blur: 0,
        };
        assert_eq!(without_set(&bg(&["a", "b"]), "a"), Some(bg(&["b"])));
        assert_eq!(without_set(&bg(&["a"]), "a"), None);
        let none = Background {
            fill: Fill::None,
            dim: 0,
            blur: 0,
        };
        assert_eq!(without_set(&none, "a"), Some(none.clone()));
    }
}
