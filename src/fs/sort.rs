//! Natural ordering for file names: case-insensitive, digit runs compared as numbers
//! ("IMG_2" before "IMG_10"), German umlauts and common accents sorted with their base
//! letter (DIN 5007-1: "ä" like "a").

use std::cmp::Ordering;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Part {
    /// Number value, then digit count (so "02" sorts after "2" on a tie).
    Num(u128, usize),
    Text(String),
}

fn fold(c: char, out: &mut String) {
    let base = match c {
        'ä' | 'Ä' | 'à' | 'á' | 'â' | 'ã' | 'å' | 'À' | 'Á' | 'Â' | 'Ã' | 'Å' => 'a',
        'ö' | 'Ö' | 'ò' | 'ó' | 'ô' | 'õ' | 'ø' | 'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ø' => 'o',
        'ü' | 'Ü' | 'ù' | 'ú' | 'û' | 'Ù' | 'Ú' | 'Û' => 'u',
        'è' | 'é' | 'ê' | 'ë' | 'È' | 'É' | 'Ê' | 'Ë' => 'e',
        'ì' | 'í' | 'î' | 'ï' | 'Ì' | 'Í' | 'Î' | 'Ï' => 'i',
        'ç' | 'Ç' => 'c',
        'ñ' | 'Ñ' => 'n',
        'ß' => {
            out.push_str("ss");
            return;
        }
        other => {
            out.extend(other.to_lowercase());
            return;
        }
    };
    out.push(base);
}

/// Sort key for one name.
pub fn key(name: &str) -> Vec<Part> {
    let mut parts = Vec::new();
    let mut chars = name.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            let mut digits = String::new();
            while let Some(&d) = chars.peek().filter(|d| d.is_ascii_digit()) {
                digits.push(d);
                chars.next();
            }
            let trimmed = digits.trim_start_matches('0');
            match if trimmed.is_empty() {
                Ok(0)
            } else {
                trimmed.parse::<u128>()
            } {
                Ok(n) => parts.push(Part::Num(n, digits.len())),
                Err(_) => parts.push(Part::Text(digits)),
            }
        } else {
            let mut text = String::new();
            while let Some(&d) = chars.peek().filter(|d| !d.is_ascii_digit()) {
                fold(d, &mut text);
                chars.next();
            }
            parts.push(Part::Text(text));
        }
    }
    parts
}

pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    key(a).cmp(&key(b)).then_with(|| a.cmp(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(mut v: Vec<&str>) -> Vec<&str> {
        v.sort_by(|a, b| natural_cmp(a, b));
        v
    }

    #[test]
    fn numbers_and_case() {
        assert_eq!(
            sorted(vec!["IMG_10.jpg", "img_2.jpg", "IMG_1.jpg"]),
            ["IMG_1.jpg", "img_2.jpg", "IMG_10.jpg"]
        );
        assert_eq!(sorted(vec!["b", "A", "a"]), ["A", "a", "b"]);
        assert_eq!(
            sorted(vec!["file02", "file2", "file1"]),
            ["file1", "file2", "file02"]
        );
    }

    #[test]
    fn umlauts_sort_with_base_letter() {
        assert_eq!(
            sorted(vec!["Zebra", "Äpfel", "Apfel", "Birne"]),
            ["Apfel", "Äpfel", "Birne", "Zebra"]
        );
        assert_eq!(
            sorted(vec!["Strasse 2", "Straße 1"]),
            ["Straße 1", "Strasse 2"]
        );
    }

    #[test]
    fn huge_digit_runs_dont_panic() {
        let long = "9".repeat(60);
        assert_eq!(sorted(vec![&long, "1"]), ["1", long.as_str()]);
    }
}
