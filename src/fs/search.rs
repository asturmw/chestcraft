//! Matching names for the search (`web::search`): without case or accents, every word of
//! the query somewhere in the name, in any order. The folder filter in `assets/browser.js`
//! folds the same way (NFKD, combining marks dropped, lower case, "ß" as "ss"), so typing
//! and pressing Enter agree.

use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

/// Longest query that is searched for (longer ones are cut, at a character boundary).
pub const MAX_QUERY: usize = 200;

/// `s` without case and accents: "Gehäuse_Deckel" → "gehause_deckel", "Straße" → "strasse".
pub fn fold(s: &str) -> String {
    let folded: String = s
        .nfkd()
        .filter(|c| !is_combining_mark(*c))
        .flat_map(char::to_lowercase)
        .collect();
    if folded.contains('ß') {
        folded.replace('ß', "ss")
    } else {
        folded
    }
}

/// The words of a query, folded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Matcher {
    words: Vec<String>,
}

impl Matcher {
    /// None for a query without words (empty or only spaces): nothing to search for.
    pub fn new(query: &str) -> Option<Matcher> {
        let query = match query.char_indices().nth(MAX_QUERY) {
            Some((at, _)) => &query[..at],
            None => query,
        };
        let mut words: Vec<String> = fold(query).split_whitespace().map(String::from).collect();
        words.sort();
        words.dedup();
        // "foto" is in every name that holds "fotos": the longer word decides.
        let all = words.clone();
        words.retain(|w| !all.iter().any(|o| o != w && o.contains(w.as_str())));
        (!words.is_empty()).then_some(Matcher { words })
    }

    pub fn matches(&self, name: &str) -> bool {
        let name = fold(name);
        self.words.iter().all(|w| name.contains(w.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_case_accents_and_sharp_s() {
        assert_eq!(fold("Gehäuse_Deckel.STL"), "gehause_deckel.stl");
        assert_eq!(fold("Crème Brûlée"), "creme brulee");
        assert_eq!(fold("Straße"), "strasse");
        assert_eq!(fold("STRASSE"), "strasse");
        // Decomposed names (as macOS writes them) fold like composed ones.
        assert_eq!(fold("Ha\u{0308}user"), fold("Häuser"));
        assert_eq!(fold("ﬁle"), "file");
    }

    #[test]
    fn every_word_somewhere_in_any_order() {
        let m = Matcher::new("  deckel  GEHAUSE ").unwrap();
        assert!(m.matches("Gehäuse_Deckel.stl"));
        assert!(m.matches("deckel vom gehäuse"));
        assert!(!m.matches("Gehäuse.stl"));
        let m = Matcher::new("rechnung 2026").unwrap();
        assert!(m.matches("Rechnungen 2026"));
        assert!(m.matches("2026-03 Rechnung Strom.pdf"));
        assert!(!m.matches("Rechnungen 2025"));
        assert!(
            Matcher::new("strasse")
                .unwrap()
                .matches("Hauptstraße 4.jpg")
        );
        assert!(
            Matcher::new("straße")
                .unwrap()
                .matches("Hauptstrasse 4.jpg")
        );
    }

    #[test]
    fn no_words_no_search() {
        assert_eq!(Matcher::new(""), None);
        assert_eq!(Matcher::new(" \t "), None);
        // A word inside another one adds nothing.
        assert_eq!(
            Matcher::new("foto fotos Foto"),
            Some(Matcher {
                words: vec!["fotos".into()]
            })
        );
        // A very long query is cut, not refused.
        let long = "a".repeat(MAX_QUERY * 2);
        assert!(Matcher::new(&long).unwrap().matches(&"a".repeat(MAX_QUERY)));
    }
}
