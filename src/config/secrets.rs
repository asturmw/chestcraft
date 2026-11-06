//! Secrets: the Telegram bot token and an ntfy access token. They never go into the config
//! file. The service gets them through systemd `LoadCredential=secrets:<file>`
//! (`$CREDENTIALS_DIRECTORY/secrets`); commands run by hand read `secrets.toml` next to the
//! config file. Format:
//!
//! ```toml
//! telegram_token = "123456:ABC…"
//! ntfy_token = "tk_…"
//! ```

use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = "secrets.toml";
const CREDENTIAL_NAME: &str = "secrets";

#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secrets {
    pub telegram_token: Option<String>,
    pub ntfy_token: Option<String>,
}

impl std::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Secrets")
            .field("telegram_token", &self.telegram_token.as_ref().map(|_| "…"))
            .field("ntfy_token", &self.ntfy_token.as_ref().map(|_| "…"))
            .finish()
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
struct Raw {
    telegram_token: String,
    ntfy_token: String,
}

/// `<digits>:<35 or so of A–Z a–z 0–9 _ ->`, as BotFather hands them out.
pub fn valid_telegram_token(t: &str) -> bool {
    match t.split_once(':') {
        Some((id, key)) => {
            !id.is_empty()
                && id.len() <= 20
                && id.bytes().all(|b| b.is_ascii_digit())
                && (20..=100).contains(&key.len())
                && key
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        }
        None => false,
    }
}

fn valid_ntfy_token(t: &str) -> bool {
    (1..=200).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_graphic() && b != b'"')
}

impl Secrets {
    /// Parse the secrets file. Errors never quote the file's content.
    pub fn parse(text: &str) -> Result<Secrets, String> {
        let raw: Raw = toml::from_str(text).map_err(|e| match e.span() {
            Some(span) => {
                let line = text[..span.start.min(text.len())].lines().count().max(1);
                format!("syntax error or unknown key on line {line}")
            }
            None => "syntax error".to_string(),
        })?;
        let telegram = raw.telegram_token.trim();
        let ntfy = raw.ntfy_token.trim();
        if !telegram.is_empty() && !valid_telegram_token(telegram) {
            return Err("telegram_token doesn't look like a bot token (\"<id>:<key>\")".into());
        }
        if !ntfy.is_empty() && !valid_ntfy_token(ntfy) {
            return Err("ntfy_token has characters a token can't have".into());
        }
        Ok(Secrets {
            telegram_token: (!telegram.is_empty()).then(|| telegram.to_string()),
            ntfy_token: (!ntfy.is_empty()).then(|| ntfy.to_string()),
        })
    }

    /// Where the secrets come from: the systemd credential if the service has one, else
    /// `secrets.toml` in `config_dir`.
    pub fn location(config_dir: Option<&Path>) -> Option<PathBuf> {
        if let Some(dir) = std::env::var_os("CREDENTIALS_DIRECTORY") {
            let p = Path::new(&dir).join(CREDENTIAL_NAME);
            if p.exists() {
                return Some(p);
            }
        }
        config_dir.map(|d| d.join(FILE_NAME))
    }

    /// Load from `path`; a missing file means no secrets. The file must not be readable by
    /// others (systemd credentials are 0400 already).
    pub fn load(path: &Path) -> Result<Secrets, String> {
        use std::os::unix::fs::MetadataExt;
        let meta = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Secrets::default()),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        if meta.mode() & 0o007 != 0 {
            return Err(format!(
                "{}: readable by everyone; chmod 600 it (owner root)",
                path.display()
            ));
        }
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Secrets::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "123456789:AAH-example_key_with_enough_length0";

    #[test]
    fn parse_and_validate() {
        let s = Secrets::parse(&format!("telegram_token = \"{TOKEN}\"\n")).unwrap();
        assert_eq!(s.telegram_token.as_deref(), Some(TOKEN));
        assert_eq!(s.ntfy_token, None);
        assert_eq!(Secrets::parse("").unwrap(), Secrets::default());
        let e = Secrets::parse("telegram_token = \"hunter2\"\n").unwrap_err();
        assert!(!e.contains("hunter2"), "{e}");
        let e = Secrets::parse("password = \"hunter2\"\n").unwrap_err();
        assert!(!e.contains("hunter2") && e.contains("line 1"), "{e}");
        assert!(Secrets::parse("ntfy_token = \"tk_abc\"\n").is_ok());
        assert!(
            !format!(
                "{:?}",
                Secrets::parse(&format!("telegram_token = \"{TOKEN}\"")).unwrap()
            )
            .contains("AAH")
        );
    }

    #[test]
    fn file_must_be_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(FILE_NAME);
        assert_eq!(Secrets::load(&p).unwrap(), Secrets::default());
        std::fs::write(&p, format!("telegram_token = \"{TOKEN}\"\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(Secrets::load(&p).unwrap_err().contains("chmod 600"));
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(Secrets::load(&p).unwrap().telegram_token.is_some());
    }
}
