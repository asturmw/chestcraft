//! Passphrases: four random words from a list in the creator's language, joined by `-`
//! (`tanne-kiesel-ofen-raupe`). English uses the EFF short word list, German a list written
//! for chestcraft; more languages come from `[shares.wordlists]`. See `words/README.md`.

use crate::auth::random;
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

const EN: &str = include_str!("words/en.txt");
const DE: &str = include_str!("words/de.txt");

/// Words per passphrase.
pub const WORDS: usize = 4;
/// Smallest list accepted from a file (4 words of 256 still give 32 bits; with argon2 and
/// the sign-in backoff that's out of reach online).
pub const MIN_LIST: usize = 256;

/// Words of a list: one per line, the last whitespace-separated field counts (dice-numbered
/// lists work as they are), `#` starts a comment. Entries with anything but letters are
/// skipped (`-` separates the words), as are duplicates.
pub fn parse(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or_default();
        let Some(word) = line.split_whitespace().last() else {
            continue;
        };
        let word = word.to_lowercase();
        let n = word.chars().count();
        if (2..=16).contains(&n)
            && word.chars().all(char::is_alphabetic)
            && seen.insert(word.clone())
        {
            out.push(word);
        }
    }
    out
}

/// A list from a file must have at least [`MIN_LIST`] usable words.
pub fn check(text: &str) -> Result<Vec<String>, String> {
    let words = parse(text);
    if words.len() < MIN_LIST {
        return Err(format!(
            "{} usable words; a passphrase list needs at least {MIN_LIST}",
            words.len()
        ));
    }
    Ok(words)
}

pub struct WordLists {
    lists: BTreeMap<String, Vec<String>>,
}

impl WordLists {
    pub fn builtin() -> WordLists {
        let mut lists = BTreeMap::new();
        lists.insert("en".to_string(), parse(EN));
        lists.insert("de".to_string(), parse(DE));
        WordLists { lists }
    }

    /// The built-in lists plus the configured ones; a file that can't be used is skipped
    /// with a warning (the built-in list of that language, or English, takes its place).
    pub fn load(extra: &BTreeMap<String, PathBuf>) -> (WordLists, Vec<String>) {
        let mut wl = WordLists::builtin();
        let mut warnings = Vec::new();
        for (lang, path) in extra {
            match std::fs::read_to_string(path)
                .map_err(|e| e.to_string())
                .and_then(|t| check(&t))
            {
                Ok(words) => {
                    wl.lists.insert(lang.clone(), words);
                }
                Err(e) => warnings.push(format!(
                    "shares.wordlists.{lang}: {}: {e}; using the built-in list",
                    path.display()
                )),
            }
        }
        (wl, warnings)
    }

    /// The list for a language code ("pt-BR" falls back to "pt", then English).
    fn list(&self, lang: &str) -> &[String] {
        let base = lang.split('-').next().unwrap_or(lang);
        self.lists
            .get(lang)
            .or_else(|| self.lists.get(base))
            .or_else(|| self.lists.get("en"))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub fn passphrase(&self, lang: &str) -> String {
        let list = self.list(lang);
        (0..WORDS)
            .map(|_| list[random::index(list.len())].as_str())
            .collect::<Vec<_>>()
            .join("-")
    }
}

/// What a visitor typed, in the form passphrases are stored: lowercase, words joined by
/// single `-` (phones capitalize the first letter; people type spaces).
pub fn normalize(input: &str) -> String {
    input
        .to_lowercase()
        .split(|c: char| c.is_whitespace() || matches!(c, '-' | '_' | '.' | ',' | '/'))
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_lists_are_large_and_clean() {
        let wl = WordLists::builtin();
        for (lang, min) in [("en", 1290), ("de", 1100)] {
            let list = wl.list(lang);
            assert!(list.len() >= min, "{lang}: {}", list.len());
            assert!(
                list.iter()
                    .all(|w| w.chars().all(|c| c.is_ascii_lowercase()) && w.len() >= 3),
                "{lang}"
            );
        }
        assert!(!wl.list("en").iter().any(|w| w.contains('-')));
        assert!(wl.list("de").iter().any(|w| w == "tanne"));
        assert_eq!(wl.list("de-AT")[0], wl.list("de")[0]);
        assert_eq!(wl.list("fr")[0], wl.list("en")[0]);
    }

    #[test]
    fn passphrases_have_four_words_from_the_list() {
        let wl = WordLists::builtin();
        let p = wl.passphrase("de");
        let words: Vec<&str> = p.split('-').collect();
        assert_eq!(words.len(), WORDS, "{p}");
        assert!(words.iter().all(|w| wl.list("de").iter().any(|l| l == w)));
        assert_ne!(wl.passphrase("en"), wl.passphrase("en"));
        assert_eq!(normalize(p.as_str()), p);
    }

    #[test]
    fn typed_passphrases_are_normalized() {
        assert_eq!(
            normalize("  Tanne Kiesel-ofen  RAUPE "),
            "tanne-kiesel-ofen-raupe"
        );
        assert_eq!(
            normalize("tanne--kiesel_ofen.raupe"),
            "tanne-kiesel-ofen-raupe"
        );
    }

    #[test]
    fn list_files() {
        let text = "# comment\n11111\tapfel\n11112 birne # fruit\nyo-yo\nApfel\n\n";
        assert_eq!(parse(text), ["apfel", "birne"]);
        assert!(check(text).unwrap_err().contains("at least"));
        // "worta", "wortb", … one word per number, digits spelled as letters.
        let many: String = (0..300)
            .map(|i: u32| {
                let tail: String = i
                    .to_string()
                    .bytes()
                    .map(|d| char::from(b'a' + (d - b'0')))
                    .collect();
                format!("wort{tail}\n")
            })
            .collect();
        assert!(check(&many).is_ok());
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("fr.txt");
        std::fs::write(&good, &many).unwrap();
        let mut extra = BTreeMap::new();
        extra.insert("fr".to_string(), good);
        extra.insert("it".to_string(), dir.path().join("missing.txt"));
        let (wl, warnings) = WordLists::load(&extra);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(wl.passphrase("fr").starts_with("wort"));
        assert!(!wl.passphrase("it").starts_with("wort"));
    }
}
