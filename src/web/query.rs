//! Query strings with repeated keys (`?zip&n=a&n=b`), decoded like HTML forms.

use percent_encoding::percent_decode_str;

#[derive(Debug, Default)]
pub struct Query {
    pairs: Vec<(String, String)>,
}

fn decode(s: &str) -> String {
    percent_decode_str(&s.replace('+', " "))
        .decode_utf8_lossy()
        .into_owned()
}

impl Query {
    pub fn parse(q: Option<&str>) -> Query {
        let pairs = q
            .unwrap_or("")
            .split('&')
            .filter(|s| !s.is_empty())
            .map(|kv| match kv.split_once('=') {
                Some((k, v)) => (decode(k), decode(v)),
                None => (decode(kv), String::new()),
            })
            .collect();
        Query { pairs }
    }

    pub fn has(&self, key: &str) -> bool {
        self.pairs.iter().any(|(k, _)| k == key)
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.pairs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    pub fn all<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a str> {
        self.pairs
            .iter()
            .filter(move |(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_keys_and_decoding() {
        let q = Query::parse(Some("zip&n=Urlaub+2025&n=Stra%C3%9Fe.jpg&page=2"));
        assert!(q.has("zip"));
        assert_eq!(q.get("zip"), Some(""));
        assert_eq!(
            q.all("n").collect::<Vec<_>>(),
            ["Urlaub 2025", "Straße.jpg"]
        );
        assert_eq!(q.get("page"), Some("2"));
        assert!(!Query::parse(None).has("zip"));
    }
}
