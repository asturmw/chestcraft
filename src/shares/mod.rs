//! Share links and public uploads.
//!
//! A share gives visitors who know its password access to one folder or file at
//! `/s/<id>`: `view` (browse, download, zip), `view_upload` (also upload) or `upload`
//! (upload only; a visitor sees nothing but their own uploads). A share never grants more
//! than its creator has: the creator's rights are checked on every request, as seen from
//! the internet, so a share only works on volumes reachable from there. Visitor uploads
//! never overwrite (a taken name becomes `name (1)`).
//!
//! Lifecycle: a share whose creator is disabled or lost the rights, whose volume is gone, or
//! whose target is in the trash is paused and works again once that is undone. Moves and
//! renames done through chestcraft take the share along; a target that vanished some other
//! way is "missing". Revoked and expired shares keep their record and access log.
//!
//! Public uploads: the admin opens uploads into a public volume with a password, for a
//! while. Visitors who enter it may upload and create folders there, never overwrite.
//!
//! Both kinds of visitors get a guest session (12 hours) after entering the password.

pub mod words;

use crate::auth::perm::{Origin, Perms, Principal, path_perms};
use crate::auth::random::BASE58;
use crate::auth::{password, random, session};
use crate::config::Config;
use crate::db::{Db, DbError, GuestSession, PublicWrite, Share, User};
use crate::fs::path::RelPath;
use crate::fs::volume::{Kind, Volumes};
use std::collections::HashMap;
use std::io;
use std::net::IpAddr;

pub const ID_LEN: usize = 12;

/// Cookie of a share visitor (path `/s/<id>`).
pub const SHARE_COOKIE: &str = "cc_share";
/// Cookie of a public uploader (path `/`).
pub const PUBLIC_COOKIE: &str = "cc_public";
/// Guest sessions end this long after the password was entered (seconds).
pub const GUEST_SECS: i64 = 12 * 3600;
/// Minimum length of a password the creator picks.
pub const MIN_OWN_PASSWORD: usize = 8;
pub const MAX_NOTE_CHARS: usize = 2000;
/// Shares one account may have that aren't revoked.
pub const MAX_PER_USER: usize = 1000;
/// A guest session lists at most this many uploads.
pub const MAX_LISTED_UPLOADS: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    View,
    ViewUpload,
    Upload,
}

impl Mode {
    pub const ALL: [Mode; 3] = [Mode::View, Mode::ViewUpload, Mode::Upload];

    pub fn parse(s: &str) -> Option<Mode> {
        Some(match s {
            "view" => Mode::View,
            "view_upload" => Mode::ViewUpload,
            "upload" => Mode::Upload,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::View => "view",
            Mode::ViewUpload => "view_upload",
            Mode::Upload => "upload",
        }
    }

    /// What a visitor may do.
    pub fn grants(self) -> Perms {
        match self {
            Mode::View => Perms::READ,
            Mode::ViewUpload => Perms::READ | Perms::WRITE,
            Mode::Upload => Perms::WRITE,
        }
    }

    /// What the creator needs at the target.
    pub fn needs(self) -> Perms {
        match self {
            Mode::View => Perms::READ | Perms::SHARE,
            Mode::ViewUpload | Mode::Upload => Perms::READ | Perms::WRITE | Perms::SHARE,
        }
    }

    pub fn uploads(self) -> bool {
        self != Mode::View
    }
}

/// How long a share or a public upload stays open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expiry {
    Day,
    Week,
    Month,
    Never,
}

impl Expiry {
    pub const ALL: [Expiry; 4] = [Expiry::Day, Expiry::Week, Expiry::Month, Expiry::Never];

    pub fn parse(s: &str) -> Option<Expiry> {
        Some(match s {
            "1d" => Expiry::Day,
            "7d" => Expiry::Week,
            "30d" => Expiry::Month,
            "never" => Expiry::Never,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Expiry::Day => "1d",
            Expiry::Week => "7d",
            Expiry::Month => "30d",
            Expiry::Never => "never",
        }
    }

    pub fn until(self, now: i64) -> Option<i64> {
        let days = match self {
            Expiry::Day => 1,
            Expiry::Week => 7,
            Expiry::Month => 30,
            Expiry::Never => return None,
        };
        Some(now + days * 86_400)
    }
}

/// Why a share is paused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pause {
    /// The creator's account is disabled or gone.
    Owner,
    /// The creator lost the rights the share needs, or the volume is closed to the internet.
    Rights,
    /// The volume was removed from the config.
    Volume,
    /// The target is in the trash.
    Trash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Active,
    Expired,
    Revoked,
    Paused(Pause),
    /// The target isn't where the share points, and not in the trash.
    Missing,
}

impl Status {
    pub fn code(self) -> &'static str {
        match self {
            Status::Active => "active",
            Status::Expired => "expired",
            Status::Revoked => "revoked",
            Status::Paused(_) => "paused",
            Status::Missing => "missing",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ShareError {
    #[error("no such volume")]
    NoVolume,
    #[error("the file or folder doesn't exist")]
    NotFound,
    #[error("the volume isn't reachable from the internet")]
    NotRemote,
    #[error("the creator's rights don't allow this share")]
    NoRights,
    #[error("uploads need a folder")]
    UploadNeedsFolder,
    #[error("the password needs at least {MIN_OWN_PASSWORD} characters")]
    PasswordTooShort,
    #[error("the note is too long")]
    NoteTooLong,
    #[error("too many shares")]
    TooMany,
    #[error("{0}")]
    Db(#[from] DbError),
}

pub fn new_id() -> String {
    random::string(BASE58, ID_LEN)
}

pub fn plausible_id(s: &str) -> bool {
    s.len() == ID_LEN && s.bytes().all(|b| BASE58.contains(&b))
}

/// The password a share or public upload gets.
#[derive(Debug, Clone)]
pub enum Password {
    /// Generated: stored normalized, checked without case and separators.
    Passphrase(String),
    /// Picked by the creator, at least [`MIN_OWN_PASSWORD`] characters, checked exactly.
    Own(String),
}

impl Password {
    pub fn text(&self) -> &str {
        match self {
            Password::Passphrase(p) | Password::Own(p) => p,
        }
    }

    pub fn check(&self) -> Result<(), ShareError> {
        match self {
            Password::Own(p) if p.chars().count() < MIN_OWN_PASSWORD => {
                Err(ShareError::PasswordTooShort)
            }
            Password::Passphrase(p) if words::normalize(p).len() < MIN_OWN_PASSWORD => {
                Err(ShareError::PasswordTooShort)
            }
            _ => Ok(()),
        }
    }

    /// (argon2id hash, is a passphrase)
    pub fn hash(&self) -> (String, bool) {
        match self {
            Password::Passphrase(p) => (password::hash(&words::normalize(p)), true),
            Password::Own(p) => (password::hash(p), false),
        }
    }
}

/// Check what a visitor typed. Blocks (argon2).
pub fn verify_password(input: &str, hash: &str, passphrase: bool) -> bool {
    if passphrase {
        password::verify(&words::normalize(input), hash)
    } else {
        password::verify(input, hash)
    }
}

/// The creator as a principal (their role decides what the share may do).
pub fn owner_principal(user: &User) -> Principal {
    Principal::from_account(&user.name, &user.role)
}

/// May `owner` create (or keep) a share of `path` in `volume` with `mode`?
pub fn check_rights(
    cfg: &Config,
    owner: &Principal,
    volume: &str,
    path: &RelPath,
    mode: Mode,
) -> Result<(), ShareError> {
    let vol = cfg.volumes.get(volume).ok_or(ShareError::NoVolume)?;
    if !vol.remote {
        return Err(ShareError::NotRemote);
    }
    let perms = path_perms(cfg, owner, Origin::Remote, volume, path);
    if !perms.contains(mode.needs()) {
        return Err(ShareError::NoRights);
    }
    Ok(())
}

pub struct NewShare<'a> {
    pub owner: &'a User,
    pub volume: &'a str,
    pub path: &'a RelPath,
    pub mode: Mode,
    pub expiry: Expiry,
    pub password: &'a Password,
    pub note: &'a str,
    /// Largest visitor upload (upload modes only).
    pub max_file: Option<u64>,
}

pub fn clean_note(note: &str) -> Result<String, ShareError> {
    let note: String = note
        .trim()
        .chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .collect();
    if note.chars().count() > MAX_NOTE_CHARS {
        return Err(ShareError::NoteTooLong);
    }
    Ok(note)
}

/// Is the target there, and is it a folder? `None` if missing.
fn target_kind(vols: &Volumes, volume: &str, path: &RelPath) -> io::Result<Option<bool>> {
    let Some(vol) = vols.get(volume) else {
        return Ok(None);
    };
    match vol.stat(path) {
        Ok(m) => Ok(match m.kind {
            Kind::Dir => Some(true),
            Kind::File => Some(false),
            Kind::Other => None,
        }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Create a share. Blocks (stat, argon2, database).
pub fn create(
    cfg: &Config,
    vols: &Volumes,
    db: &Db,
    req: &NewShare,
    now: i64,
) -> Result<Share, ShareError> {
    let principal = owner_principal(req.owner);
    check_rights(cfg, &principal, req.volume, req.path, req.mode)?;
    let is_dir = target_kind(vols, req.volume, req.path)
        .ok()
        .flatten()
        .ok_or(ShareError::NotFound)?;
    if req.mode.uploads() && !is_dir {
        return Err(ShareError::UploadNeedsFolder);
    }
    req.password.check()?;
    let note = clean_note(req.note)?;
    let owner_key = User::key(&req.owner.name);
    let live = db
        .shares()?
        .iter()
        .filter(|s| s.owner == owner_key && s.revoked.is_none())
        .count();
    if live >= MAX_PER_USER {
        return Err(ShareError::TooMany);
    }
    let id = loop {
        let id = new_id();
        if db.share(&id)?.is_none() {
            break id;
        }
    };
    let (password_hash, passphrase) = req.password.hash();
    let share = Share {
        id,
        owner: owner_key,
        volume: req.volume.to_string(),
        path: req.path.as_str().to_string(),
        is_dir,
        mode: req.mode.as_str().to_string(),
        password_hash,
        passphrase,
        note,
        created: now,
        expires: req.expiry.until(now),
        max_file: req.max_file.filter(|_| req.mode.uploads()),
        revoked: None,
        opens: 0,
        downloads: 0,
        uploads: 0,
        last_opened: None,
    };
    db.put_share(&share)?;
    Ok(share)
}

/// The state of a share right now, and its creator's account. Blocks (stat, database).
pub fn status(
    cfg: &Config,
    vols: &Volumes,
    db: &Db,
    share: &Share,
    now: i64,
) -> Result<(Status, Option<User>), DbError> {
    let owner = db.user(&share.owner)?;
    if share.revoked.is_some() {
        return Ok((Status::Revoked, owner));
    }
    if share.expires.is_some_and(|t| t <= now) {
        return Ok((Status::Expired, owner));
    }
    let Some(mode) = Mode::parse(&share.mode) else {
        return Ok((Status::Paused(Pause::Rights), owner));
    };
    if !cfg.volumes.contains_key(&share.volume) || vols.get(&share.volume).is_none() {
        return Ok((Status::Paused(Pause::Volume), owner));
    }
    let Some(user) = owner.as_ref().filter(|u| !u.disabled) else {
        return Ok((Status::Paused(Pause::Owner), owner));
    };
    let Ok(path) = RelPath::parse(&share.path) else {
        return Ok((Status::Missing, owner));
    };
    if check_rights(cfg, &owner_principal(user), &share.volume, &path, mode).is_err() {
        return Ok((Status::Paused(Pause::Rights), owner));
    }
    let kind = target_kind(vols, &share.volume, &path).unwrap_or_else(|e| {
        tracing::warn!(share = %share.id, volume = %share.volume, path = %share.path, "can't check a share's target: {e}");
        None
    });
    if kind == Some(share.is_dir) {
        return Ok((Status::Active, owner));
    }
    let in_trash = kind.is_none()
        && db.trash_items()?.iter().any(|t| {
            t.volume == share.volume
                && RelPath::parse(&t.path).is_ok_and(|tp| !tp.is_root() && path.starts_with(&tp))
        });
    Ok((
        if in_trash {
            Status::Paused(Pause::Trash)
        } else {
            Status::Missing
        },
        owner,
    ))
}

/// What a visitor may do at `sub` (relative to the share's target): the mode, limited by
/// the creator's rights there. Dot-names are never visible to visitors.
pub fn visitor_perms(cfg: &Config, owner: &Principal, share: &Share, sub: &RelPath) -> Perms {
    if sub.is_hidden() {
        return Perms::empty();
    }
    let (Some(mode), Ok(root)) = (Mode::parse(&share.mode), RelPath::parse(&share.path)) else {
        return Perms::empty();
    };
    let Ok(abs) = root.join_path(sub) else {
        return Perms::empty();
    };
    let creator = path_perms(cfg, owner, Origin::Remote, &share.volume, &abs);
    let mut p = Perms::empty();
    for bit in [Perms::READ, Perms::WRITE] {
        if mode.grants().contains(bit) && creator.contains(bit) {
            p |= bit;
        }
    }
    p
}

/// Shares pointing at `from` (or below it) in `from_vol` now point at `to` in `to_vol`: the
/// entry was moved or renamed through chestcraft. Returns how many shares moved.
pub fn follow_move(
    db: &Db,
    from_vol: &str,
    from: &RelPath,
    to_vol: &str,
    to: &RelPath,
) -> Result<usize, DbError> {
    if from_vol == to_vol && from == to {
        return Ok(0);
    }
    db.update_shares(|s| {
        if s.volume != from_vol {
            return false;
        }
        let Some(rest) = RelPath::parse(&s.path)
            .ok()
            .and_then(|p| p.strip_prefix(from))
        else {
            return false;
        };
        let Ok(new) = to.join_path(&rest) else {
            return false;
        };
        s.volume = to_vol.to_string();
        s.path = new.as_str().to_string();
        true
    })
}

/// Names in `dir` of `volume` that have a share that isn't revoked or expired (for the
/// badge in listings), with the time the last of their shares runs out (None: one never
/// does).
pub fn shared_names(
    shares: &[Share],
    volume: &str,
    dir: &RelPath,
    now: i64,
) -> HashMap<String, Option<i64>> {
    let mut out: HashMap<String, Option<i64>> = HashMap::new();
    let live = shares
        .iter()
        .filter(|s| s.volume == volume && s.revoked.is_none() && s.expires.is_none_or(|t| t > now));
    for s in live {
        let Ok(p) = RelPath::parse(&s.path) else {
            continue;
        };
        let (Some(parent), Some(name)) = (p.parent(), p.name()) else {
            continue;
        };
        if parent != *dir {
            continue;
        }
        out.entry(name.to_string())
            .and_modify(|until| {
                *until = match (*until, s.expires) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    _ => None,
                }
            })
            .or_insert(s.expires);
    }
    out
}

// ---------------------------------------------------------------------------------------
// Guest sessions

pub fn share_scope(id: &str) -> String {
    format!("share:{id}")
}

pub fn public_scope(volume: &str) -> String {
    format!("public:{volume}")
}

pub struct NewGuest {
    pub cookie_value: String,
    pub hash: String,
    pub session: GuestSession,
}

pub fn new_guest(scope: String, ip: IpAddr, now: i64) -> NewGuest {
    let cookie_value = random::hex(&random::bytes::<32>());
    NewGuest {
        hash: session::hash_id(&cookie_value),
        cookie_value,
        session: GuestSession {
            scope,
            created: now,
            csrf: random::hex(&random::bytes::<16>()),
            ip: ip.to_string(),
            uploads: Vec::new(),
        },
    }
}

pub fn guest_valid(s: &GuestSession, scope: &str, now: i64) -> bool {
    s.scope == scope && s.created <= now && now - s.created < GUEST_SECS
}

pub fn guest_cookie(name: &str, value: &str, path: &str, max_age: i64, secure: bool) -> String {
    let secure = if secure { "; Secure" } else { "" };
    format!("{name}={value}; Path={path}; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}")
}

/// End every guest session of `scope` (password changed, revoked, closed).
pub fn end_guests(db: &Db, scope: &str) -> Result<usize, DbError> {
    db.retain_guests(|s| s.scope != scope)
}

/// Remove guest sessions that ran out.
pub fn sweep_guests(db: &Db, now: i64) -> usize {
    db.retain_guests(|s| now - s.created < GUEST_SECS)
        .unwrap_or_default()
}

/// Are public uploads into `pw.volume` open right now?
pub fn public_open(cfg: &Config, pw: &PublicWrite, now: i64) -> bool {
    pw.until.is_none_or(|t| t > now) && cfg.volumes.get(&pw.volume).is_some_and(|v| v.public)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{FILE_NAME, TrashItem, UserSettings};
    use std::path::PathBuf;

    struct T {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        cfg: Config,
        vols: Volumes,
        db: Db,
    }

    fn setup() -> T {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        for d in ["dok/Rechnungen/2026", "dok/.privat", "dok/archiv", "home"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(root.join("dok/Angebot.pdf"), b"pdf").unwrap();
        let cfg = Config::from_toml_str(&format!(
            r#"
            [server]
            public_url = "https://files.example.org"
            [volumes.Dokumente]
            path = "{0}/dok"
            read_only_paths = ["archiv"]
            [volumes.Daheim]
            path = "{0}/home"
            remote = false
            [roles.familie]
            Dokumente = "rwmds"
            Daheim = "rwmds"
            [roles.leser]
            Dokumente = "rs"
            [roles.ohne]
            Dokumente = "r"
            "#,
            root.display()
        ))
        .unwrap();
        let vols = Volumes::open(&cfg);
        let db = Db::open(&root.join(FILE_NAME)).unwrap();
        T {
            _tmp: tmp,
            root,
            cfg,
            vols,
            db,
        }
    }

    fn user(name: &str, role: &str) -> User {
        User {
            name: name.into(),
            role: role.into(),
            password_hash: "x".into(),
            must_change_password: false,
            disabled: false,
            created: 0,
            last_login: None,
            settings: UserSettings::default(),
            known_remote_ips: vec![],
            kind: Default::default(),
        }
    }

    fn p(s: &str) -> RelPath {
        RelPath::parse(s).unwrap()
    }

    impl T {
        fn make(
            &self,
            owner: &User,
            vol: &str,
            path: &str,
            mode: Mode,
        ) -> Result<Share, ShareError> {
            self.db.put_user(owner).unwrap();
            create(
                &self.cfg,
                &self.vols,
                &self.db,
                &NewShare {
                    owner,
                    volume: vol,
                    path: &p(path),
                    mode,
                    expiry: Expiry::Month,
                    password: &Password::Passphrase("tanne-kiesel-ofen-raupe".into()),
                    note: " Hallo ",
                    max_file: Some(10),
                },
                1000,
            )
        }

        fn status(&self, s: &Share, now: i64) -> Status {
            let fresh = self.db.share(&s.id).unwrap().unwrap();
            status(&self.cfg, &self.vols, &self.db, &fresh, now)
                .unwrap()
                .0
        }
    }

    #[test]
    fn create_checks_rights_volume_and_target() {
        let t = setup();
        let jana = user("Jana", "familie");
        let s = t
            .make(&jana, "Dokumente", "Rechnungen", Mode::Upload)
            .unwrap();
        assert!(plausible_id(&s.id));
        assert_eq!(
            (s.owner.as_str(), s.is_dir, s.note.as_str(), s.max_file),
            ("jana", true, "Hallo", Some(10))
        );
        assert_eq!(s.expires, Some(1000 + 30 * 86_400));
        assert!(s.passphrase);
        assert!(verify_password(
            "Tanne Kiesel ofen-RAUPE",
            &s.password_hash,
            true
        ));
        assert!(!verify_password(
            "tanne-kiesel-ofen",
            &s.password_hash,
            true
        ));

        let file = t
            .make(&jana, "Dokumente", "Angebot.pdf", Mode::View)
            .unwrap();
        assert!(!file.is_dir && file.max_file.is_none());
        assert!(matches!(
            t.make(&jana, "Dokumente", "Angebot.pdf", Mode::Upload),
            Err(ShareError::UploadNeedsFolder)
        ));
        assert!(matches!(
            t.make(&jana, "Dokumente", "fehlt", Mode::View),
            Err(ShareError::NotFound)
        ));
        assert!(matches!(
            t.make(&jana, "Daheim", "", Mode::View),
            Err(ShareError::NotRemote)
        ));
        assert!(matches!(
            t.make(&jana, "Nirgends", "", Mode::View),
            Err(ShareError::NoVolume)
        ));
        // Never above the creator's rights.
        let leser = user("Mika", "leser");
        assert!(
            t.make(&leser, "Dokumente", "Rechnungen", Mode::View)
                .is_ok()
        );
        assert!(matches!(
            t.make(&leser, "Dokumente", "Rechnungen", Mode::ViewUpload),
            Err(ShareError::NoRights)
        ));
        assert!(matches!(
            t.make(&user("Ole", "ohne"), "Dokumente", "", Mode::View),
            Err(ShareError::NoRights)
        ));
        // Read-only areas can be shared for viewing, not for uploads; dot-folders only by
        // the admin.
        assert!(t.make(&jana, "Dokumente", "archiv", Mode::View).is_ok());
        assert!(matches!(
            t.make(&jana, "Dokumente", "archiv", Mode::Upload),
            Err(ShareError::NoRights)
        ));
        assert!(matches!(
            t.make(&jana, "Dokumente", ".privat", Mode::View),
            Err(ShareError::NoRights)
        ));
        assert!(
            t.make(&user("Admin", "admin"), "Dokumente", ".privat", Mode::View)
                .is_ok()
        );
    }

    #[test]
    fn own_passwords_need_eight_characters_and_match_exactly() {
        assert!(matches!(
            Password::Own("kurz".into()).check(),
            Err(ShareError::PasswordTooShort)
        ));
        let (h, pp) = Password::Own("Geheim 123".into()).hash();
        assert!(!pp);
        assert!(verify_password("Geheim 123", &h, false));
        assert!(!verify_password("geheim 123", &h, false));
    }

    #[test]
    fn status_follows_the_lifecycle() {
        let t = setup();
        let mut jana = user("Jana", "familie");
        let s = t
            .make(&jana, "Dokumente", "Rechnungen", Mode::View)
            .unwrap();
        assert_eq!(t.status(&s, 2000), Status::Active);
        assert_eq!(t.status(&s, 1000 + 30 * 86_400), Status::Expired);

        jana.disabled = true;
        t.db.put_user(&jana).unwrap();
        assert_eq!(t.status(&s, 2000), Status::Paused(Pause::Owner));
        jana.disabled = false;
        jana.role = "ohne".into();
        t.db.put_user(&jana).unwrap();
        assert_eq!(t.status(&s, 2000), Status::Paused(Pause::Rights));
        jana.role = "familie".into();
        t.db.put_user(&jana).unwrap();

        // In the trash (the folder or one above it) → paused; restored → active again.
        std::fs::rename(t.root.join("dok/Rechnungen"), t.root.join("weg")).unwrap();
        assert_eq!(t.status(&s, 2000), Status::Missing);
        t.db.put_trash(&TrashItem {
            id: "t1".into(),
            volume: "Dokumente".into(),
            path: "Rechnungen".into(),
            fs_root: String::new(),
            is_dir: true,
            size: 0,
            files: 0,
            deleted_at: 1,
            deleted_by: "user:jana".into(),
            reason: "delete".into(),
        })
        .unwrap();
        assert_eq!(t.status(&s, 2000), Status::Paused(Pause::Trash));
        std::fs::rename(t.root.join("weg"), t.root.join("dok/Rechnungen")).unwrap();
        assert_eq!(t.status(&s, 2000), Status::Active);

        // A file where a folder was shared is not the target.
        std::fs::rename(t.root.join("dok/Rechnungen"), t.root.join("weg")).unwrap();
        std::fs::write(t.root.join("dok/Rechnungen"), b"x").unwrap();
        t.db.delete_trash("t1").unwrap();
        assert_eq!(t.status(&s, 2000), Status::Missing);

        t.db.update_share(&s.id, true, |s| s.revoked = Some(5))
            .unwrap();
        assert_eq!(t.status(&s, 2000), Status::Revoked);
    }

    #[test]
    fn visitors_get_the_mode_within_the_creators_rights() {
        let t = setup();
        let jana = user("Jana", "familie");
        let owner = owner_principal(&jana);
        let s = t.make(&jana, "Dokumente", "", Mode::ViewUpload).unwrap();
        assert_eq!(
            visitor_perms(&t.cfg, &owner, &s, &p("Rechnungen/2026")),
            Perms::READ | Perms::WRITE
        );
        assert_eq!(visitor_perms(&t.cfg, &owner, &s, &p("archiv")), Perms::READ);
        assert_eq!(
            visitor_perms(&t.cfg, &owner, &s, &p(".privat")),
            Perms::empty()
        );
        let up = t
            .make(&jana, "Dokumente", "Rechnungen", Mode::Upload)
            .unwrap();
        assert_eq!(visitor_perms(&t.cfg, &owner, &up, &p("")), Perms::WRITE);
        let admin = user("Admin", "admin");
        let hidden = t.make(&admin, "Dokumente", ".privat", Mode::View).unwrap();
        assert_eq!(
            visitor_perms(&t.cfg, &owner_principal(&admin), &hidden, &p("")),
            Perms::READ,
            "the shared dot-folder itself, but nothing hidden below it"
        );
    }

    #[test]
    fn shares_follow_moves_and_badges_find_them() {
        let t = setup();
        let jana = user("Jana", "familie");
        let a = t
            .make(&jana, "Dokumente", "Rechnungen", Mode::View)
            .unwrap();
        let b = t
            .make(&jana, "Dokumente", "Rechnungen/2026", Mode::View)
            .unwrap();
        let c = t
            .make(&jana, "Dokumente", "Angebot.pdf", Mode::View)
            .unwrap();
        assert_eq!(
            follow_move(
                &t.db,
                "Dokumente",
                &p("Rechnungen"),
                "Dokumente",
                &p("Belege/Rechnungen")
            )
            .unwrap(),
            2
        );
        let path = |s: &Share| t.db.share(&s.id).unwrap().unwrap().path;
        assert_eq!(path(&a), "Belege/Rechnungen");
        assert_eq!(path(&b), "Belege/Rechnungen/2026");
        assert_eq!(path(&c), "Angebot.pdf");
        // "Angebot.pdf2" is not below "Angebot.pdf".
        assert_eq!(
            follow_move(&t.db, "Dokumente", &p("Angebot.pdf2"), "X", &p("y")).unwrap(),
            0
        );

        let all = t.db.shares().unwrap();
        let names = shared_names(&all, "Dokumente", &p(""), 2000);
        assert_eq!(names.keys().collect::<Vec<_>>(), ["Angebot.pdf"]);
        let names = shared_names(&all, "Dokumente", &p("Belege"), 2000);
        assert_eq!(names.keys().collect::<Vec<_>>(), ["Rechnungen"]);
        assert!(shared_names(&all, "Dokumente", &p(""), 1000 + 31 * 86_400).is_empty());
    }

    #[test]
    fn guest_sessions_expire_and_end_by_scope() {
        let t = setup();
        let ip: IpAddr = "203.0.113.7".parse().unwrap();
        let g = new_guest(share_scope("abc"), ip, 100);
        assert!(guest_valid(&g.session, "share:abc", 100 + GUEST_SECS - 1));
        assert!(!guest_valid(&g.session, "share:abc", 100 + GUEST_SECS));
        assert!(!guest_valid(&g.session, "share:other", 100));
        t.db.put_guest(&g.hash, &g.session).unwrap();
        let h = new_guest(public_scope("Austausch"), ip, 100);
        t.db.put_guest(&h.hash, &h.session).unwrap();
        assert_eq!(end_guests(&t.db, "share:abc").unwrap(), 1);
        assert!(t.db.guest(&h.hash).unwrap().is_some());
        assert_eq!(sweep_guests(&t.db, 100 + GUEST_SECS), 1);
        assert!(guest_cookie(SHARE_COOKIE, "v", "/s/abc", 60, true).contains("Path=/s/abc;"));
    }
}
