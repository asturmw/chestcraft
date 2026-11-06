//! Alternative names for name clashes: `Angebot (1).pdf` when keeping both, and
//! `Angebot (restored).pdf` when a restored item's name is taken again.

use crate::fs::path::MAX_NAME_BYTES;

/// Extensions kept together when a number goes in front of them.
const DOUBLE_EXTENSIONS: &[&str] = &[".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst", ".tar.lz4"];

/// Split a file name into stem and extension (with its dot). Dotfiles like `.bashrc` have no
/// extension; `backup.tar.gz` keeps `.tar.gz` together.
pub fn split_ext(name: &str) -> (&str, &str) {
    let lower = name.to_ascii_lowercase();
    for ext in DOUBLE_EXTENSIONS {
        if lower.len() > ext.len() && lower.ends_with(ext) {
            let i = name.len() - ext.len();
            return (&name[..i], &name[i..]);
        }
    }
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// The longest prefix of `s` with at most `max` bytes, cut at a character boundary.
fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// `name` with ` (tag)` before the extension (folders: at the end), shortened so the result
/// stays a valid name.
fn tagged(name: &str, is_dir: bool, tag: &str) -> String {
    let suffix = format!(" ({tag})");
    let (stem, ext) = if is_dir { (name, "") } else { split_ext(name) };
    let (stem, ext) = match MAX_NAME_BYTES.checked_sub(suffix.len() + ext.len()) {
        Some(budget) if budget > 0 => (truncate(stem, budget), ext),
        // An absurdly long extension: treat the whole name as the stem.
        _ => (truncate(name, MAX_NAME_BYTES - suffix.len()), ""),
    };
    format!("{stem}{suffix}{ext}")
}

/// How to name the alternatives when a name is taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    /// `name (1).ext`, `name (2).ext`, …
    Numbered,
    /// `name (restored).ext`, `name (restored 2).ext`, …
    Restored,
}

/// The `n`-th alternative for `name` (`n` ≥ 1).
pub fn alternative(name: &str, is_dir: bool, variant: Variant, n: u32) -> String {
    match variant {
        Variant::Numbered => tagged(name, is_dir, &n.to_string()),
        Variant::Restored if n == 1 => tagged(name, is_dir, "restored"),
        Variant::Restored => tagged(name, is_dir, &format!("restored {n}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::path::validate_new_name;
    use proptest::prelude::*;

    #[test]
    fn numbered_names() {
        let n = |name, dir, i| alternative(name, dir, Variant::Numbered, i);
        assert_eq!(n("Angebot.pdf", false, 1), "Angebot (1).pdf");
        assert_eq!(n("Angebot.pdf", false, 12), "Angebot (12).pdf");
        assert_eq!(n("README", false, 1), "README (1)");
        assert_eq!(n(".bashrc", false, 1), ".bashrc (1)");
        assert_eq!(n("backup.TAR.GZ", false, 2), "backup (2).TAR.GZ");
        assert_eq!(n("Urlaub.2025", true, 1), "Urlaub.2025 (1)");
    }

    #[test]
    fn restored_names() {
        let r = |i| alternative("Rechnung.pdf", false, Variant::Restored, i);
        assert_eq!(r(1), "Rechnung (restored).pdf");
        assert_eq!(r(2), "Rechnung (restored 2).pdf");
    }

    #[test]
    fn long_names_are_shortened_at_character_boundaries() {
        let long = format!("{}.jpg", "ä".repeat(126));
        let alt = alternative(&long, false, Variant::Numbered, 1);
        assert!(
            alt.len() <= MAX_NAME_BYTES && alt.ends_with(" (1).jpg"),
            "{alt}"
        );
        let ext = format!("a.{}", "x".repeat(254));
        let alt = alternative(&ext, false, Variant::Numbered, 1);
        assert!(
            alt.len() <= MAX_NAME_BYTES && alt.ends_with(" (1)"),
            "{alt}"
        );
    }

    proptest! {
        #[test]
        fn alternatives_are_valid_and_differ(
            name in "[^/\\p{Cc}]{1,120}|[a-z.]{200,255}",
            n in 1u32..10_000,
            dir: bool,
        ) {
            prop_assume!(name.len() <= MAX_NAME_BYTES && validate_new_name(&name).is_ok());
            for v in [Variant::Numbered, Variant::Restored] {
                let alt = alternative(&name, dir, v, n);
                prop_assert!(alt.len() <= MAX_NAME_BYTES);
                prop_assert_ne!(&alt, &name);
                prop_assert!(validate_new_name(&alt).is_ok(), "{}", alt);
            }
        }
    }
}
