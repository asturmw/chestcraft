pub mod apppass;
pub mod backoff;
pub mod password;
pub mod perm;
pub mod random;
pub mod session;

/// Account names: 1–32 of a–z, A–Z, 0–9, `.`, `_`, `-`, not starting with `.` or `-`.
pub fn valid_username(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && !name.starts_with(['.', '-'])
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}
