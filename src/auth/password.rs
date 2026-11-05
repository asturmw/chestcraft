//! Account passwords: argon2id with the RustCrypto defaults (19 MiB, 2 passes, 1 lane).

use super::random;
use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use std::sync::LazyLock;

/// Minimum length for account passwords.
pub const MIN_ACCOUNT_PASSWORD: usize = 10;

pub fn hash(password: &str) -> String {
    Argon2::default()
        .hash_password(password.as_bytes())
        .expect("argon2 hashing")
        .to_string()
}

pub fn verify(password: &str, phc: &str) -> bool {
    PasswordHash::new(phc).is_ok_and(|h| {
        Argon2::default()
            .verify_password(password.as_bytes(), &h)
            .is_ok()
    })
}

/// Costs as much as a real check, so unknown accounts can't be told apart by timing.
pub fn verify_dummy(password: &str) {
    static DUMMY: LazyLock<String> = LazyLock::new(|| hash("timing equalizer, not a password"));
    let _ = verify(password, &DUMMY);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weak {
    TooShort,
    SameAsName,
}

pub fn check_new(password: &str, username: &str) -> Result<(), Weak> {
    if password.chars().count() < MIN_ACCOUNT_PASSWORD {
        return Err(Weak::TooShort);
    }
    if password.trim().eq_ignore_ascii_case(username.trim()) {
        return Err(Weak::SameAsName);
    }
    Ok(())
}

/// One-time password for new or reset accounts; the owner must change it at first sign-in.
/// No 0/O, 1/l/I, so it can be read out loud.
pub fn generate() -> String {
    random::string(
        b"abcdefghjkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789",
        16,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_and_verify() {
        let h = hash("korrekt pferd batterie");
        assert!(h.starts_with("$argon2id$"));
        assert!(verify("korrekt pferd batterie", &h));
        assert!(!verify("korrekt pferd batteriE", &h));
        assert!(!verify("x", "not a phc string"));
        verify_dummy("anything");
    }

    #[test]
    fn rules_for_new_passwords() {
        assert_eq!(check_new("kurz", "jana"), Err(Weak::TooShort));
        assert_eq!(
            check_new("Jana-Jana-Jana", "jana-jana-jana"),
            Err(Weak::SameAsName)
        );
        assert!(check_new("zehn Zeichen", "jana").is_ok());
        assert_eq!(generate().len(), 16);
        assert_ne!(generate(), generate());
    }
}
