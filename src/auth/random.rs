//! Randomness from the OS (getrandom) and small encoders.

/// `N` random bytes. Panics only if the OS random source fails, which leaves nothing safe
/// to do anyway.
pub fn bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).expect("OS random source failed");
    b
}

/// Uniformly random string over `alphabet` (rejection sampling, no modulo bias).
pub fn string(alphabet: &[u8], len: usize) -> String {
    assert!(!alphabet.is_empty() && alphabet.len() <= 256);
    let n = alphabet.len() as u32;
    let limit = 256 - (256 % n);
    let mut out = String::with_capacity(len);
    while out.len() < len {
        for b in bytes::<64>() {
            if u32::from(b) < limit {
                out.push(alphabet[(u32::from(b) % n) as usize] as char);
                if out.len() == len {
                    break;
                }
            }
        }
    }
    out
}

/// Uniform index in `0..n`.
pub fn index(n: usize) -> usize {
    assert!(n > 0);
    let n = n as u64;
    let limit = u64::MAX - (u64::MAX % n);
    loop {
        let v = u64::from_le_bytes(bytes::<8>());
        if v < limit {
            return (v % n) as usize;
        }
    }
}

pub fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_use_only_the_alphabet() {
        let s = string(b"ab", 200);
        assert_eq!(s.len(), 200);
        assert!(s.chars().all(|c| c == 'a' || c == 'b'));
        assert!(s.contains('a') && s.contains('b'));
        assert_eq!(hex(&[0, 255, 16]), "00ff10");
        assert!((0..1000).all(|_| index(3) < 3));
    }
}
