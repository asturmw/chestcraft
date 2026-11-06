//! Permissions: what a principal may do at a path.
//!
//! Letters in the config: `r` list/download/preview, `w` upload/new folder, `m` rename/move,
//! `d` delete (to trash), `s` create share links. Every non-empty set must include `r`.

use crate::config::{Config, VolumeConfig};
use crate::fs::path::RelPath;
use bitflags::bitflags;

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct Perms: u8 {
        const READ = 1;
        const WRITE = 2;
        const MOVE = 4;
        const DELETE = 8;
        const SHARE = 16;
    }
}

/// Permissions that change files. Read-only areas and read-only volumes remove these.
pub const CHANGES: Perms = Perms::WRITE.union(Perms::MOVE).union(Perms::DELETE);

impl Perms {
    /// Parse a letter string such as `"rwmds"`.
    pub fn parse(s: &str) -> Result<Perms, String> {
        let mut p = Perms::empty();
        for ch in s.chars() {
            let bit = match ch {
                'r' => Perms::READ,
                'w' => Perms::WRITE,
                'm' => Perms::MOVE,
                'd' => Perms::DELETE,
                's' => Perms::SHARE,
                other => {
                    return Err(format!(
                        "unknown permission letter '{other}' (use r, w, m, d, s)"
                    ));
                }
            };
            if p.contains(bit) {
                return Err(format!("permission letter '{ch}' appears twice"));
            }
            p |= bit;
        }
        if !p.is_empty() && !p.contains(Perms::READ) {
            return Err(format!(
                "\"{s}\" needs r: every permission set starts with read access"
            ));
        }
        Ok(p)
    }

    pub fn letters(self) -> String {
        [
            (Perms::READ, 'r'),
            (Perms::WRITE, 'w'),
            (Perms::MOVE, 'm'),
            (Perms::DELETE, 'd'),
            (Perms::SHARE, 's'),
        ]
        .iter()
        .filter(|(bit, _)| self.contains(*bit))
        .map(|(_, ch)| *ch)
        .collect()
    }
}

/// Who is asking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Principal {
    Anonymous,
    /// A signed-in user with a role from the config. A role that no longer exists
    /// grants nothing.
    User {
        name: String,
        role: String,
    },
    Admin {
        name: String,
    },
}

impl Principal {
    pub fn is_admin(&self) -> bool {
        matches!(self, Principal::Admin { .. })
    }

    pub fn is_anonymous(&self) -> bool {
        matches!(self, Principal::Anonymous)
    }

    /// The username, for signed-in principals.
    pub fn name(&self) -> Option<&str> {
        match self {
            Principal::Anonymous => None,
            Principal::User { name, .. } | Principal::Admin { name } => Some(name),
        }
    }

    /// Build from a stored account: role "admin" is the built-in admin.
    pub fn from_account(name: &str, role: &str) -> Principal {
        if role == ADMIN_ROLE {
            Principal::Admin {
                name: name.to_string(),
            }
        } else {
            Principal::User {
                name: name.to_string(),
                role: role.to_string(),
            }
        }
    }
}

/// The built-in role with every permission.
pub const ADMIN_ROLE: &str = "admin";

/// Where the request came from (see `server.remote_proxies`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Local,
    Remote,
}

/// Permissions of `who` on a whole volume, before path rules.
pub fn volume_perms(cfg: &Config, who: &Principal, origin: Origin, volume: &str) -> Perms {
    let Some(vol) = cfg.volumes.get(volume) else {
        return Perms::empty();
    };
    if origin == Origin::Remote && !vol.remote {
        return Perms::empty();
    }
    let base = match who {
        Principal::Admin { .. } => Perms::all(),
        Principal::User { role, .. } => cfg
            .roles
            .get(role)
            .and_then(|r| r.volumes.get(volume))
            .copied()
            .unwrap_or_default(),
        Principal::Anonymous => Perms::empty(),
    };
    // Anyone may read a public volume.
    let base = if vol.public { base | Perms::READ } else { base };
    if vol.read_only { base - CHANGES } else { base }
}

/// Permissions of `who` at `path` inside `volume`, including read-only areas and the
/// hidden/internal path rules.
pub fn path_perms(
    cfg: &Config,
    who: &Principal,
    origin: Origin,
    volume: &str,
    path: &RelPath,
) -> Perms {
    if path.is_internal() {
        return Perms::empty();
    }
    if path.is_hidden() && !who.is_admin() {
        return Perms::empty();
    }
    let perms = volume_perms(cfg, who, origin, volume);
    match cfg.volumes.get(volume) {
        Some(vol) if in_read_only_area(vol, path) => perms - CHANGES,
        _ => perms,
    }
}

/// Permissions of an anonymous visitor who entered the public upload password of `volume`:
/// read like everyone, plus upload and new folders where the volume may change. Never
/// rename, move or delete.
pub fn public_writer_perms(cfg: &Config, origin: Origin, volume: &str, path: &RelPath) -> Perms {
    let read = path_perms(cfg, &Principal::Anonymous, origin, volume, path);
    match cfg.volumes.get(volume) {
        Some(vol) if vol.public && read.contains(Perms::READ) && !in_read_only_area(vol, path) => {
            read | Perms::WRITE
        }
        _ => read,
    }
}

/// Is `path` inside (or equal to) one of the volume's read-only areas?
pub fn in_read_only_area(vol: &VolumeConfig, path: &RelPath) -> bool {
    vol.read_only || vol.read_only_paths.iter().any(|ro| path.starts_with(ro))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn cfg() -> Config {
        Config::from_toml_str(
            r#"
            [server]
            public_url = "https://files.example.org"
            trusted_proxies = ["192.0.2.10"]
            remote_proxies = ["192.0.2.10"]

            [volumes.Medien]
            path = "/srv/medien"
            read_only_paths = ["immich"]

            [volumes.Animes]
            path = "/srv/medien/video/anime"

            [volumes.Share]
            path = "/srv/share"
            public = true

            [volumes.Backups]
            path = "/srv/backups"
            remote = false

            [volumes.Archiv]
            path = "/srv/archiv"
            read_only = true

            [roles.anime]
            Animes = "r"

            [roles.familie]
            Medien = "rwmds"
            Share = "rwmd"
            Archiv = "rwmd"
            "#,
        )
        .unwrap()
    }

    fn admin() -> Principal {
        Principal::Admin {
            name: "root".into(),
        }
    }

    fn user(role: &str) -> Principal {
        Principal::User {
            name: "u".into(),
            role: role.into(),
        }
    }

    fn p(s: &str) -> RelPath {
        RelPath::parse(s).unwrap()
    }

    #[test]
    fn parse_letters() {
        assert_eq!(Perms::parse("rwmds").unwrap(), Perms::all());
        assert_eq!(Perms::parse("").unwrap(), Perms::empty());
        assert!(Perms::parse("w").unwrap_err().contains("needs r"));
        assert!(Perms::parse("rr").unwrap_err().contains("twice"));
        assert!(Perms::parse("rx").unwrap_err().contains("unknown"));
        assert_eq!(Perms::parse("rws").unwrap().letters(), "rws");
    }

    #[test]
    fn role_grants_only_its_volumes() {
        let c = cfg();
        assert_eq!(
            volume_perms(&c, &user("anime"), Origin::Local, "Animes"),
            Perms::READ
        );
        assert_eq!(
            volume_perms(&c, &user("anime"), Origin::Local, "Medien"),
            Perms::empty()
        );
        assert_eq!(
            volume_perms(&c, &user("missing-role"), Origin::Local, "Animes"),
            Perms::empty()
        );
        assert_eq!(
            volume_perms(&c, &admin(), Origin::Local, "Nope"),
            Perms::empty()
        );
    }

    #[test]
    fn public_volume_is_readable_by_anyone() {
        let c = cfg();
        assert_eq!(
            volume_perms(&c, &Principal::Anonymous, Origin::Remote, "Share"),
            Perms::READ
        );
        assert_eq!(
            volume_perms(&c, &user("anime"), Origin::Remote, "Share"),
            Perms::READ
        );
        assert_eq!(
            volume_perms(&c, &Principal::Anonymous, Origin::Local, "Medien"),
            Perms::empty()
        );
        // A role keeps its own rights on a public volume.
        assert_eq!(
            volume_perms(&c, &user("familie"), Origin::Local, "Share"),
            Perms::parse("rwmd").unwrap()
        );
    }

    #[test]
    fn local_only_volume_is_closed_from_the_internet_even_for_admin() {
        let c = cfg();
        assert_eq!(
            volume_perms(&c, &admin(), Origin::Remote, "Backups"),
            Perms::empty()
        );
        assert_eq!(
            volume_perms(&c, &admin(), Origin::Local, "Backups"),
            Perms::all()
        );
    }

    #[test]
    fn read_only_areas_strip_changes_but_keep_read_and_share() {
        let c = cfg();
        let fam = user("familie");
        let ro = path_perms(&c, &fam, Origin::Local, "Medien", &p("immich/lib/x.jpg"));
        assert_eq!(ro, Perms::READ | Perms::SHARE);
        let ro_root = path_perms(&c, &fam, Origin::Local, "Medien", &p("immich"));
        assert_eq!(ro_root, Perms::READ | Perms::SHARE);
        let rw = path_perms(&c, &fam, Origin::Local, "Medien", &p("immich-export/x.jpg"));
        assert_eq!(rw, Perms::all());
        let whole = path_perms(&c, &admin(), Origin::Local, "Archiv", &p("a/b"));
        assert_eq!(whole, Perms::READ | Perms::SHARE);
    }

    #[test]
    fn public_writers_may_only_add_to_public_volumes() {
        let c = cfg();
        let w = |vol: &str, path: &str| public_writer_perms(&c, Origin::Remote, vol, &p(path));
        assert_eq!(w("Share", "Eingang"), Perms::READ | Perms::WRITE);
        assert_eq!(w("Share", ".versteckt"), Perms::empty());
        assert_eq!(w("Medien", ""), Perms::empty());
        assert_eq!(w("Archiv", ""), Perms::empty());
    }

    #[test]
    fn hidden_paths_are_admin_only_and_internal_paths_closed_for_all() {
        let c = cfg();
        let fam = user("familie");
        assert_eq!(
            path_perms(&c, &fam, Origin::Local, "Medien", &p("a/.git/config")),
            Perms::empty()
        );
        assert_eq!(
            path_perms(&c, &admin(), Origin::Local, "Medien", &p("a/.git/config")),
            Perms::all()
        );
        for internal in [".chestcraft-trash/1/x", "a/.hist/up2k.db"] {
            assert_eq!(
                path_perms(&c, &admin(), Origin::Local, "Medien", &p(internal)),
                Perms::empty(),
                "{internal}"
            );
        }
    }
}
