//! UI strings and locale formats.
//!
//! English and German are compiled in. More languages load from `<dir>/<code>.toml`
//! (the `i18n` folder next to the config file); a file named like a built-in language is
//! merged over it. Missing keys fall back to English, then to the key itself.
//!
//! Catalog format: TOML sections become key prefixes (`[browser] empty = "…"` is
//! `browser.empty`). `[meta] name` is the language's own name; `[format]` holds the decimal
//! separator, the date pattern and month abbreviations.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

/// Index into the loaded catalogs. English is always index 0, German 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Lang(usize);

pub const EN: Lang = Lang(0);
pub const DE: Lang = Lang(1);

const BUILTIN: [(&str, &str); 2] = [
    ("en", include_str!("../i18n/en.toml")),
    ("de", include_str!("../i18n/de.toml")),
];

pub struct Catalog {
    pub code: String,
    strings: HashMap<String, String>,
    months: Vec<String>,
}

pub struct I18n {
    catalogs: Vec<Catalog>,
}

fn flatten(
    prefix: &str,
    table: &toml::Table,
    out: &mut HashMap<String, String>,
    months: &mut Vec<String>,
) {
    for (k, v) in table {
        let key = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        match v {
            toml::Value::String(s) => {
                out.insert(key, s.clone());
            }
            toml::Value::Table(t) => flatten(&key, t, out, months),
            toml::Value::Array(a) if key == "format.months" => {
                *months = a
                    .iter()
                    .filter_map(|m| m.as_str().map(String::from))
                    .collect();
            }
            _ => {}
        }
    }
}

fn parse_catalog(code: &str, text: &str) -> Result<Catalog, String> {
    let table: toml::Table = toml::from_str(text).map_err(|e| format!("{code}: {e}"))?;
    let mut strings = HashMap::new();
    let mut months = Vec::new();
    flatten("", &table, &mut strings, &mut months);
    Ok(Catalog {
        code: code.to_string(),
        strings,
        months,
    })
}

impl I18n {
    /// Only the built-in languages.
    pub fn builtin() -> I18n {
        let catalogs = BUILTIN
            .iter()
            .map(|(code, text)| parse_catalog(code, text).expect("built-in catalog parses"))
            .collect();
        I18n { catalogs }
    }

    /// Built-ins plus every `<code>.toml` in `dir`. Broken files are logged and skipped.
    pub fn load(dir: Option<&Path>) -> I18n {
        let mut i18n = I18n::builtin();
        let Some(dir) = dir else { return i18n };
        let Ok(entries) = std::fs::read_dir(dir) else {
            return i18n;
        };
        let mut files: Vec<_> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
        files.sort();
        for path in files {
            let Some(code) = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".toml"))
                .filter(|c| crate::config::is_language_code(c))
            else {
                continue;
            };
            let parsed = std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|t| parse_catalog(code, &t));
            match parsed {
                Ok(cat) => match i18n.catalogs.iter_mut().find(|c| c.code == cat.code) {
                    Some(existing) => {
                        existing.strings.extend(cat.strings);
                        if !cat.months.is_empty() {
                            existing.months = cat.months;
                        }
                    }
                    None => i18n.catalogs.push(cat),
                },
                Err(e) => tracing::warn!("skipping language file {}: {e}", path.display()),
            }
        }
        i18n
    }

    pub fn code(&self, lang: Lang) -> &str {
        &self.catalogs[lang.0].code
    }

    pub fn by_code(&self, code: &str) -> Option<Lang> {
        self.catalogs.iter().position(|c| c.code == code).map(Lang)
    }

    /// `(lang, code, own name)` for language switchers.
    pub fn languages(&self) -> impl Iterator<Item = (Lang, &str, &str)> {
        self.catalogs.iter().enumerate().map(|(i, c)| {
            let name = c
                .strings
                .get("meta.name")
                .map(String::as_str)
                .unwrap_or(&c.code);
            (Lang(i), c.code.as_str(), name)
        })
    }

    /// The string for `key`, falling back to English, then to the key.
    pub fn t<'a>(&'a self, lang: Lang, key: &'a str) -> &'a str {
        self.catalogs[lang.0]
            .strings
            .get(key)
            .or_else(|| self.catalogs[EN.0].strings.get(key))
            .map(String::as_str)
            .unwrap_or_else(|| {
                debug_assert!(false, "missing i18n key {key}");
                key
            })
    }

    /// Does `lang` (or the English fallback) have `key`?
    pub fn has(&self, lang: Lang, key: &str) -> bool {
        self.catalogs[lang.0].strings.contains_key(key)
            || self.catalogs[EN.0].strings.contains_key(key)
    }

    /// `t` with `{placeholder}` substitution.
    pub fn tf(&self, lang: Lang, key: &str, args: &[(&str, &str)]) -> String {
        substitute(self.t(lang, key), args)
    }

    /// Plural form: `key_one` for 1, `key_other` otherwise; `{count}` is filled in.
    pub fn tn(&self, lang: Lang, key: &str, count: u64, args: &[(&str, &str)]) -> String {
        let form = if count == 1 {
            format!("{key}_one")
        } else {
            format!("{key}_other")
        };
        let count_s = self.number(lang, count);
        let mut all: Vec<(&str, &str)> = vec![("count", &count_s)];
        all.extend_from_slice(args);
        substitute(self.t(lang, &form), &all)
    }

    /// Pick the best available language from an `Accept-Language` header; English if none fits.
    pub fn negotiate(&self, accept_language: Option<&str>) -> Lang {
        let mut best: Option<(f32, Lang)> = None;
        for item in accept_language.unwrap_or("").split(',') {
            let mut parts = item.trim().split(';');
            let tag = parts.next().unwrap_or("").trim();
            if tag.is_empty() || tag == "*" {
                continue;
            }
            let q = parts
                .find_map(|p| {
                    p.trim()
                        .strip_prefix("q=")
                        .and_then(|v| v.parse::<f32>().ok())
                })
                .unwrap_or(1.0);
            let primary = tag.split('-').next().unwrap_or(tag).to_ascii_lowercase();
            let found = self.by_code(tag).or_else(|| self.by_code(&primary));
            if let Some(lang) = found
                && q > 0.0
                && best.is_none_or(|(bq, _)| q > bq)
            {
                best = Some((q, lang));
            }
        }
        best.map(|(_, l)| l).unwrap_or(EN)
    }

    fn decimal(&self, lang: Lang) -> &str {
        self.t(lang, "format.decimal")
    }

    /// Whole number with the language's grouping separator.
    pub fn number(&self, lang: Lang, n: u64) -> String {
        let group = self.t(lang, "format.group");
        let digits = n.to_string();
        let mut out = String::new();
        for (i, ch) in digits.chars().enumerate() {
            if i > 0 && (digits.len() - i).is_multiple_of(3) {
                out.push_str(group);
            }
            out.push(ch);
        }
        out
    }

    /// Size in decimal (SI) units: "2,4 GB" / "2.4 GB", "512 B".
    pub fn size(&self, lang: Lang, bytes: u64) -> String {
        const UNITS: [&str; 6] = ["B", "kB", "MB", "GB", "TB", "PB"];
        if bytes < 1000 {
            return format!("{bytes} B");
        }
        let mut v = bytes as f64;
        let mut unit = 0;
        while v >= 1000.0 && unit < UNITS.len() - 1 {
            v /= 1000.0;
            unit += 1;
        }
        let text = if v < 100.0 {
            format!("{v:.1}")
        } else {
            format!("{v:.0}")
        };
        format!("{} {}", text.replace('.', self.decimal(lang)), UNITS[unit])
    }

    /// Date and time in the language's pattern, e.g. "05.11.2026, 18:30" or
    /// "5 Nov 2026, 18:30". Pattern tokens: {d} {d2} {m2} {mon} {y} {H} {M}.
    pub fn datetime(&self, lang: Lang, unix_secs: i64, tz: &jiff::tz::TimeZone) -> String {
        let Ok(ts) = jiff::Timestamp::from_second(unix_secs) else {
            return String::new();
        };
        let z = ts.to_zoned(tz.clone());
        let months = if self.catalogs[lang.0].months.len() == 12 {
            &self.catalogs[lang.0].months
        } else {
            &self.catalogs[EN.0].months
        };
        let mon = months
            .get(z.month() as usize - 1)
            .map(String::as_str)
            .unwrap_or("");
        let mut out = String::new();
        let pattern = self.t(lang, "format.datetime");
        let mut rest = pattern;
        while let Some(start) = rest.find('{') {
            out.push_str(&rest[..start]);
            let Some(end) = rest[start..].find('}') else {
                break;
            };
            let token = &rest[start + 1..start + end];
            let _ = match token {
                "d" => write!(out, "{}", z.day()),
                "d2" => write!(out, "{:02}", z.day()),
                "m2" => write!(out, "{:02}", z.month()),
                "mon" => write!(out, "{mon}"),
                "y" => write!(out, "{}", z.year()),
                "H" => write!(out, "{:02}", z.hour()),
                "M" => write!(out, "{:02}", z.minute()),
                other => write!(out, "{{{other}}}"),
            };
            rest = &rest[start + end + 1..];
        }
        out.push_str(rest);
        out
    }
}

fn substitute(template: &str, args: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (name, value) in args {
        out = out.replace(&format!("{{{name}}}"), value);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn keys(c: &Catalog) -> BTreeSet<&str> {
        c.strings.keys().map(String::as_str).collect()
    }

    fn placeholders(s: &str) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        let mut rest = s;
        while let Some(a) = rest.find('{') {
            let Some(b) = rest[a..].find('}') else { break };
            out.insert(rest[a + 1..a + b].to_string());
            rest = &rest[a + b + 1..];
        }
        out
    }

    #[test]
    fn builtin_catalogs_have_the_same_keys_and_placeholders() {
        let i = I18n::builtin();
        let (en, de) = (&i.catalogs[0], &i.catalogs[1]);
        assert_eq!(
            keys(en),
            keys(de),
            "en.toml and de.toml must have identical keys"
        );
        for (k, v) in &en.strings {
            assert!(!v.trim().is_empty(), "empty string for {k}");
            if k != "format.datetime" {
                assert_eq!(
                    placeholders(v),
                    placeholders(&de.strings[k]),
                    "placeholders differ for {k}"
                );
            }
        }
        assert_eq!(en.months.len(), 12);
        assert_eq!(de.months.len(), 12);
    }

    #[test]
    fn negotiation() {
        let i = I18n::builtin();
        assert_eq!(i.negotiate(Some("de-DE,de;q=0.9,en;q=0.8")), DE);
        assert_eq!(i.negotiate(Some("fr-FR,fr;q=0.9,en;q=0.5,de;q=0.4")), EN);
        assert_eq!(i.negotiate(Some("fr")), EN);
        assert_eq!(i.negotiate(None), EN);
        assert_eq!(i.negotiate(Some("en;q=0,de")), DE);
    }

    #[test]
    fn formats() {
        let i = I18n::builtin();
        assert_eq!(i.size(DE, 2_400_000_000_000), "2,4 TB");
        assert_eq!(i.size(EN, 2_400_000_000_000), "2.4 TB");
        assert_eq!(i.size(EN, 512), "512 B");
        assert_eq!(i.size(EN, 123_456_789), "123 MB");
        assert_eq!(i.number(DE, 1_234_567), "1.234.567");
        assert_eq!(i.number(EN, 1_234_567), "1,234,567");
        let tz = jiff::tz::TimeZone::get("Europe/Berlin").unwrap();
        // 2026-11-05 17:30 UTC is 18:30 in Berlin (CET).
        let ts = jiff::civil::date(2026, 11, 5)
            .at(17, 30, 0, 0)
            .to_zoned(jiff::tz::TimeZone::UTC)
            .unwrap()
            .timestamp()
            .as_second();
        assert_eq!(i.datetime(DE, ts, &tz), "05.11.2026, 18:30");
        assert_eq!(i.datetime(EN, ts, &tz), "5 Nov 2026, 18:30");
    }

    #[test]
    fn extra_language_files_and_fallback() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("fr.toml"),
            "[meta]\nname = \"Français\"\n[common]\nsign_in = \"Se connecter\"\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("broken.toml"), "not = [valid").unwrap();
        std::fs::write(
            dir.path().join("de.toml"),
            "[common]\nsign_in = \"Einloggen\"\n",
        )
        .unwrap();
        let i = I18n::load(Some(dir.path()));
        let fr = i.by_code("fr").unwrap();
        assert_eq!(i.t(fr, "common.sign_in"), "Se connecter");
        assert_eq!(
            i.t(fr, "common.home"),
            i.t(EN, "common.home"),
            "falls back to English"
        );
        assert_eq!(
            i.t(DE, "common.sign_in"),
            "Einloggen",
            "file merges over the built-in"
        );
        assert_eq!(i.negotiate(Some("fr-CA,fr;q=0.9")), fr);
        assert!(
            i.languages()
                .any(|(_, code, name)| code == "fr" && name == "Français")
        );
    }

    #[test]
    fn plurals() {
        let i = I18n::builtin();
        assert_eq!(i.tn(EN, "browser.items", 1, &[]), "1 item");
        assert_eq!(i.tn(DE, "browser.items", 1200, &[]), "1.200 Einträge");
    }
}
