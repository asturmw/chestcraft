//! Validated paths relative to a volume or share root.
//!
//! A [`RelPath`] never contains `.` or `..` components, empty components, NUL or `/`
//! inside a component, so joining it to a root can't climb out lexically. Symlink
//! containment is enforced separately when files are opened (`openat2` with
//! `RESOLVE_BENEATH`).

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use std::fmt;

/// Longest single name ZFS and most Linux filesystems accept.
pub const MAX_NAME_BYTES: usize = 255;
/// Longest relative path we accept.
pub const MAX_PATH_BYTES: usize = 4096;

/// Characters left unescaped in URL path segments.
const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    #[error("empty name")]
    Empty,
    #[error("'.' and '..' are not allowed as names")]
    DotSegment,
    #[error("name contains a character that is not allowed")]
    ForbiddenChar,
    #[error("name is reserved for chestcraft")]
    Reserved,
    #[error("name is longer than {MAX_NAME_BYTES} bytes")]
    NameTooLong,
    #[error("path is longer than {MAX_PATH_BYTES} bytes")]
    PathTooLong,
    #[error("invalid percent-encoding or UTF-8 in path")]
    BadEncoding,
}

/// Names that belong to chestcraft or copyparty internals. Never listed, never reachable
/// through the UI or WebDAV.
pub fn is_internal_name(name: &str) -> bool {
    name == ".hist" || name.starts_with(".chestcraft")
}

/// Rules for names that already exist on disk.
pub fn validate_existing_name(name: &str) -> Result<(), PathError> {
    if name.is_empty() {
        Err(PathError::Empty)
    } else if name == "." || name == ".." {
        Err(PathError::DotSegment)
    } else if name.contains('/') || name.contains('\0') {
        Err(PathError::ForbiddenChar)
    } else if name.len() > MAX_NAME_BYTES {
        Err(PathError::NameTooLong)
    } else {
        Ok(())
    }
}

/// Stricter rules for names chestcraft creates (upload, new folder, rename).
pub fn validate_new_name(name: &str) -> Result<(), PathError> {
    validate_existing_name(name)?;
    if name.chars().any(char::is_control) {
        return Err(PathError::ForbiddenChar);
    }
    if name.trim().is_empty() {
        return Err(PathError::Empty);
    }
    if is_internal_name(name) {
        return Err(PathError::Reserved);
    }
    Ok(())
}

/// A normalized relative path: components joined by `/`, no leading or trailing slash.
/// The empty path is the root.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RelPath(String);

impl RelPath {
    pub fn root() -> Self {
        RelPath(String::new())
    }

    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Parse an already decoded path such as `a/b`, `/a/b/` or `` (root).
    /// Empty segments (from `//`) are skipped; `.` and `..` are rejected, not resolved.
    pub fn parse(s: &str) -> Result<Self, PathError> {
        let mut out = String::new();
        for seg in s.split('/').filter(|s| !s.is_empty()) {
            validate_existing_name(seg)?;
            push_component(&mut out, seg)?;
        }
        Ok(RelPath(out))
    }

    /// Parse the percent-encoded path part of a URL. A `%2F` inside a segment is
    /// rejected rather than treated as a separator.
    pub fn from_url(raw: &str) -> Result<Self, PathError> {
        let mut out = String::new();
        for seg in raw.split('/').filter(|s| !s.is_empty()) {
            let decoded = percent_decode_str(seg)
                .decode_utf8()
                .map_err(|_| PathError::BadEncoding)?;
            validate_existing_name(&decoded)?;
            push_component(&mut out, &decoded)?;
        }
        Ok(RelPath(out))
    }

    /// Percent-encoded form for use in URLs (no leading slash).
    pub fn to_url(&self) -> String {
        self.components()
            .map(|c| utf8_percent_encode(c, SEGMENT).to_string())
            .collect::<Vec<_>>()
            .join("/")
    }

    /// Append one existing-name component.
    pub fn join(&self, name: &str) -> Result<RelPath, PathError> {
        validate_existing_name(name)?;
        let mut out = self.0.clone();
        push_component(&mut out, name)?;
        Ok(RelPath(out))
    }

    /// Append another relative path.
    pub fn join_path(&self, other: &RelPath) -> Result<RelPath, PathError> {
        let mut out = self.0.clone();
        for c in other.components() {
            push_component(&mut out, c)?;
        }
        Ok(RelPath(out))
    }

    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.0.split('/').filter(|s| !s.is_empty())
    }

    pub fn depth(&self) -> usize {
        self.components().count()
    }

    pub fn name(&self) -> Option<&str> {
        if self.is_root() {
            None
        } else {
            Some(self.0.rsplit('/').next().unwrap_or(&self.0))
        }
    }

    pub fn parent(&self) -> Option<RelPath> {
        if self.is_root() {
            return None;
        }
        Some(match self.0.rfind('/') {
            Some(i) => RelPath(self.0[..i].to_string()),
            None => RelPath::root(),
        })
    }

    /// Component-wise prefix test: `a/b/c` starts with `a/b`, but `a/bc` does not.
    pub fn starts_with(&self, prefix: &RelPath) -> bool {
        if prefix.is_root() {
            return true;
        }
        self.0 == prefix.0
            || (self.0.len() > prefix.0.len()
                && self.0.starts_with(&prefix.0)
                && self.0.as_bytes()[prefix.0.len()] == b'/')
    }

    /// The part of `self` below `prefix`, if `self` starts with it.
    pub fn strip_prefix(&self, prefix: &RelPath) -> Option<RelPath> {
        if !self.starts_with(prefix) {
            return None;
        }
        if prefix.is_root() {
            return Some(self.clone());
        }
        Some(RelPath(
            self.0[prefix.0.len()..].trim_start_matches('/').to_string(),
        ))
    }

    /// Any component starts with a dot.
    pub fn is_hidden(&self) -> bool {
        self.components().any(|c| c.starts_with('.'))
    }

    /// Any component is a chestcraft/copyparty internal name.
    pub fn is_internal(&self) -> bool {
        self.components().any(is_internal_name)
    }
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "/{}", self.0)
    }
}

fn push_component(out: &mut String, c: &str) -> Result<(), PathError> {
    if !out.is_empty() {
        out.push('/');
    }
    out.push_str(c);
    if out.len() > MAX_PATH_BYTES {
        return Err(PathError::PathTooLong);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn parse_normalizes_slashes() {
        assert_eq!(RelPath::parse("/a//b/").unwrap().as_str(), "a/b");
        assert!(RelPath::parse("").unwrap().is_root());
        assert!(RelPath::parse("///").unwrap().is_root());
    }

    #[test]
    fn rejects_dot_segments() {
        for p in ["..", "a/../b", "./a", "a/.", "/.."] {
            assert_eq!(RelPath::parse(p), Err(PathError::DotSegment), "{p}");
        }
        for p in ["%2e%2e", "a/%2E%2E/b", "%2e"] {
            assert_eq!(RelPath::from_url(p), Err(PathError::DotSegment), "{p}");
        }
    }

    #[test]
    fn rejects_encoded_separator_and_nul() {
        assert_eq!(RelPath::from_url("a%2Fb"), Err(PathError::ForbiddenChar));
        assert_eq!(RelPath::from_url("a%00b"), Err(PathError::ForbiddenChar));
        assert_eq!(RelPath::from_url("%ff"), Err(PathError::BadEncoding));
    }

    #[test]
    fn url_round_trip_with_umlauts_and_spaces() {
        let p = RelPath::parse("Urlaub Gardasee/Straße #1?.jpg").unwrap();
        let url = p.to_url();
        assert!(!url.contains(' ') && !url.contains('#') && !url.contains('?'));
        assert_eq!(RelPath::from_url(&url).unwrap(), p);
    }

    #[test]
    fn prefix_is_component_wise() {
        let ab = RelPath::parse("a/b").unwrap();
        assert!(RelPath::parse("a/b/c").unwrap().starts_with(&ab));
        assert!(RelPath::parse("a/b").unwrap().starts_with(&ab));
        assert!(!RelPath::parse("a/bc").unwrap().starts_with(&ab));
        assert!(RelPath::parse("x").unwrap().starts_with(&RelPath::root()));
        assert_eq!(
            RelPath::parse("a/b/c/d")
                .unwrap()
                .strip_prefix(&ab)
                .unwrap()
                .as_str(),
            "c/d"
        );
        assert!(RelPath::parse("a/bc").unwrap().strip_prefix(&ab).is_none());
    }

    #[test]
    fn parent_and_name() {
        let p = RelPath::parse("a/b/c.txt").unwrap();
        assert_eq!(p.name(), Some("c.txt"));
        assert_eq!(p.parent().unwrap().as_str(), "a/b");
        assert_eq!(
            RelPath::parse("a").unwrap().parent().unwrap(),
            RelPath::root()
        );
        assert_eq!(RelPath::root().parent(), None);
    }

    #[test]
    fn hidden_and_internal() {
        assert!(RelPath::parse("a/.git/config").unwrap().is_hidden());
        assert!(!RelPath::parse("a/b.txt").unwrap().is_hidden());
        assert!(
            RelPath::parse("x/.chestcraft-trash/1")
                .unwrap()
                .is_internal()
        );
        assert!(RelPath::parse(".hist/up2k.db").unwrap().is_internal());
        assert!(!RelPath::parse(".history").unwrap().is_internal());
    }

    #[test]
    fn new_name_rules() {
        assert!(validate_new_name("Rechnung 2026-014.pdf").is_ok());
        assert!(validate_new_name(".directory").is_ok());
        assert_eq!(validate_new_name("a\nb"), Err(PathError::ForbiddenChar));
        assert_eq!(validate_new_name("   "), Err(PathError::Empty));
        assert_eq!(
            validate_new_name(".chestcraft-trash"),
            Err(PathError::Reserved)
        );
        assert_eq!(
            validate_new_name(&"x".repeat(256)),
            Err(PathError::NameTooLong)
        );
    }

    fn component() -> impl Strategy<Value = String> {
        // Any non-empty string without '/' or NUL that isn't "." or "..".
        "[^/\u{0}]{1,40}".prop_filter("dot segments", |s| s != "." && s != "..")
    }

    proptest! {
        #[test]
        fn url_round_trip(parts in prop::collection::vec(component(), 0..6)) {
            let p = RelPath::parse(&parts.join("/")).unwrap();
            prop_assert_eq!(RelPath::from_url(&p.to_url()).unwrap(), p);
        }

        #[test]
        fn parsed_paths_never_escape(s in ".{0,200}") {
            if let Ok(p) = RelPath::parse(&s) {
                for c in p.components() {
                    prop_assert!(c != "." && c != "..");
                    prop_assert!(!c.contains('\0'));
                }
                let base = std::path::Path::new("/vol");
                let joined = base.join(p.as_str());
                prop_assert!(joined.starts_with(base));
                prop_assert!(joined.components().all(|c| !matches!(c, std::path::Component::ParentDir)));
            }
        }

        #[test]
        fn url_parsing_never_escapes(s in "[a-zA-Z0-9%./_~ -]{0,120}") {
            if let Ok(p) = RelPath::from_url(&s) {
                for c in p.components() {
                    prop_assert!(c != "." && c != ".." && !c.contains('/'));
                }
            }
        }

        #[test]
        fn strip_then_join_is_identity(
            a in prop::collection::vec(component(), 0..4),
            b in prop::collection::vec(component(), 0..4),
        ) {
            let pa = RelPath::parse(&a.join("/")).unwrap();
            let pb = RelPath::parse(&b.join("/")).unwrap();
            let full = pa.join_path(&pb).unwrap();
            prop_assert!(full.starts_with(&pa));
            prop_assert_eq!(pa.join_path(&full.strip_prefix(&pa).unwrap()).unwrap(), full);
        }
    }
}
