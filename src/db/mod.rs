//! The state database (redb, one file in `paths.state_dir`): accounts, sessions, the audit
//! log, the trash index and unfinished uploads. Exactly one process may open it; a second open fails with [`DbError::Locked`].
//!
//! Records are JSON so they can grow fields without migrations; `meta.schema` versions the
//! table layout. All calls block (and may fsync); async code runs them in `spawn_blocking`.

use redb::{Database, Durability, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

const META: TableDefinition<&str, &str> = TableDefinition::new("meta");
const USERS: TableDefinition<&str, &[u8]> = TableDefinition::new("users");
const SESSIONS: TableDefinition<&str, &[u8]> = TableDefinition::new("sessions");
const AUDIT: TableDefinition<u64, &[u8]> = TableDefinition::new("audit");
const TRASH: TableDefinition<&str, &[u8]> = TableDefinition::new("trash");
const UPLOADS: TableDefinition<&str, &[u8]> = TableDefinition::new("uploads");
const BATCHES: TableDefinition<&str, &[u8]> = TableDefinition::new("upload_batches");

pub const SCHEMA_VERSION: &str = "1";
pub const FILE_NAME: &str = "state.redb";

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("the database is open in another chestcraft process (stop the service first)")]
    Locked,
    #[error("database schema {0} is newer than this chestcraft understands")]
    TooNew(String),
    #[error("database: {0}")]
    Redb(String),
    #[error("database record: {0}")]
    Json(#[from] serde_json::Error),
}

impl From<redb::Error> for DbError {
    fn from(e: redb::Error) -> Self {
        match e {
            redb::Error::DatabaseAlreadyOpen => DbError::Locked,
            other => DbError::Redb(other.to_string()),
        }
    }
}

macro_rules! from_redb {
    ($($t:ty),*) => {$(
        impl From<$t> for DbError {
            fn from(e: $t) -> Self {
                redb::Error::from(e).into()
            }
        }
    )*};
}
from_redb!(
    redb::DatabaseError,
    redb::TransactionError,
    redb::TableError,
    redb::StorageError,
    redb::CommitError,
    redb::SetDurabilityError
);

pub type Result<T> = std::result::Result<T, DbError>;

/// A user account. The key is the lowercased name, so sign-in ignores case.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct User {
    /// As typed when the account was created.
    pub name: String,
    /// "admin" or a role from the config.
    pub role: String,
    /// argon2id PHC string.
    pub password_hash: String,
    #[serde(default)]
    pub must_change_password: bool,
    #[serde(default)]
    pub disabled: bool,
    pub created: i64,
    #[serde(default)]
    pub last_login: Option<i64>,
    #[serde(default)]
    pub settings: UserSettings,
    /// Remote addresses this account signed in from, newest last (admin "new IP" alerts).
    #[serde(default)]
    pub known_remote_ips: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UserSettings {
    pub lang: Option<String>,
    /// "light" | "dark"; None follows the system.
    pub theme: Option<String>,
    pub skin: Option<String>,
    /// "vim" | "standard" | "off"; None means standard.
    pub keys: Option<String>,
}

impl User {
    pub fn key(name: &str) -> String {
        name.to_lowercase()
    }
}

/// A signed-in browser. Stored under the SHA-256 of the cookie value.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Session {
    /// User key.
    pub user: String,
    pub created: i64,
    pub last_seen: i64,
    /// Token that every state-changing form and API call must carry.
    pub csrf: String,
    pub ip: String,
    /// Created through a remote origin.
    pub remote: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuditEvent {
    pub time: i64,
    /// "user:<name>", "anon" or "share:<id>".
    pub actor: String,
    pub ip: String,
    pub action: String,
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub detail: String,
}

/// An item in a volume's trash: `<fs_root>/.chestcraft-trash/<id>/<name of path>`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TrashItem {
    pub id: String,
    pub volume: String,
    /// Where it was, relative to the volume.
    pub path: String,
    /// Folder (relative to the volume) that holds the `.chestcraft-trash` it is in.
    pub fs_root: String,
    pub is_dir: bool,
    /// Bytes and files (a folder's whole tree).
    pub size: u64,
    pub files: u64,
    pub deleted_at: i64,
    /// Actor: "user:<key>", later "share:<id>" or "public".
    pub deleted_by: String,
    /// "delete", "replaced" (by an upload) or "unfinished_upload".
    pub reason: String,
}

/// An unfinished upload. Data goes to `<fs_root>/.chestcraft-uploads/<id>.part`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UploadRecord {
    pub id: String,
    /// Actor that may continue it.
    pub owner: String,
    pub volume: String,
    /// Target folder, relative to the volume.
    pub dir: String,
    /// Relative path below `dir` ("a.txt" or "Folder/sub/a.txt").
    pub name: String,
    pub size: u64,
    /// Client's modification time, Unix milliseconds.
    pub mtime_ms: i64,
    /// Chunk size in bytes.
    pub chunk: u64,
    /// What to do when the name is taken: "ask", "rename", "replace" or "skip".
    pub clash: String,
    pub fs_root: String,
    #[serde(default)]
    pub batch: Option<String>,
    /// Finished into the batch's staging folder instead of the target folder.
    #[serde(default)]
    pub staged: bool,
    pub created: i64,
    pub touched: i64,
    /// Chunk ranges `[start, end)` known to be on disk (written and synced).
    #[serde(default)]
    pub have: Vec<(u32, u32)>,
}

/// A group of uploads (a folder upload). On staged volumes its files collect in
/// `<fs_root>/.chestcraft-uploads/<id>.batch/` until the batch finishes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BatchRecord {
    pub id: String,
    pub owner: String,
    pub volume: String,
    pub dir: String,
    pub fs_root: String,
    pub staged: bool,
    pub created: i64,
    pub touched: i64,
}

pub struct Db {
    db: Database,
    /// Last audit key handed out; keys are unique, increasing nanoseconds.
    audit_seq: AtomicU64,
}

fn now_nanos() -> u64 {
    jiff::Timestamp::now().as_nanosecond().max(0) as u64
}

impl Db {
    /// Open or create the database, creating tables and checking the schema version.
    pub fn open(path: &Path) -> Result<Db> {
        let db = Database::create(path)?;
        let tx = db.begin_write()?;
        {
            tx.open_table(USERS)?;
            tx.open_table(SESSIONS)?;
            tx.open_table(AUDIT)?;
            tx.open_table(TRASH)?;
            tx.open_table(UPLOADS)?;
            tx.open_table(BATCHES)?;
            let mut meta = tx.open_table(META)?;
            let current = meta.get("schema")?.map(|v| v.value().to_string());
            match current.as_deref() {
                None => {
                    meta.insert("schema", SCHEMA_VERSION)?;
                }
                Some(SCHEMA_VERSION) => {}
                Some(other) => return Err(DbError::TooNew(other.to_string())),
            }
        }
        tx.commit()?;
        Ok(Db {
            db,
            audit_seq: AtomicU64::new(0),
        })
    }

    fn get_json<T: DeserializeOwned>(
        &self,
        table: TableDefinition<&str, &[u8]>,
        key: &str,
    ) -> Result<Option<T>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(table)?;
        Ok(match t.get(key)? {
            Some(v) => Some(serde_json::from_slice(v.value())?),
            None => None,
        })
    }

    fn put_json<T: Serialize>(
        &self,
        table: TableDefinition<&str, &[u8]>,
        key: &str,
        value: &T,
        durable: bool,
    ) -> Result<()> {
        let bytes = serde_json::to_vec(value)?;
        let mut tx = self.db.begin_write()?;
        if !durable {
            tx.set_durability(Durability::None)?;
        }
        tx.open_table(table)?.insert(key, bytes.as_slice())?;
        tx.commit()?;
        Ok(())
    }

    fn all_json<T: DeserializeOwned>(&self, table: TableDefinition<&str, &[u8]>) -> Result<Vec<T>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(table)?;
        let mut out = Vec::new();
        for row in t.iter()? {
            let (_, v) = row?;
            out.push(serde_json::from_slice(v.value())?);
        }
        Ok(out)
    }

    fn delete_key(&self, table: TableDefinition<&str, &[u8]>, key: &str) -> Result<()> {
        let tx = self.db.begin_write()?;
        tx.open_table(table)?.remove(key)?;
        tx.commit()?;
        Ok(())
    }

    // --- users -----------------------------------------------------------------------

    pub fn user(&self, name: &str) -> Result<Option<User>> {
        self.get_json(USERS, &User::key(name))
    }

    pub fn put_user(&self, user: &User) -> Result<()> {
        self.put_json(USERS, &User::key(&user.name), user, true)
    }

    pub fn users(&self) -> Result<Vec<User>> {
        self.all_json(USERS)
    }

    // --- sessions --------------------------------------------------------------------

    pub fn session(&self, id_hash: &str) -> Result<Option<Session>> {
        self.get_json(SESSIONS, id_hash)
    }

    /// `durable = false` for bookkeeping like `last_seen`, which may be lost in a crash.
    pub fn put_session(&self, id_hash: &str, session: &Session, durable: bool) -> Result<()> {
        self.put_json(SESSIONS, id_hash, session, durable)
    }

    pub fn delete_session(&self, id_hash: &str) -> Result<()> {
        self.delete_key(SESSIONS, id_hash)
    }

    /// Remove sessions of `user_key` except `keep` (an id hash); returns how many.
    pub fn delete_user_sessions(&self, user_key: &str, keep: Option<&str>) -> Result<usize> {
        self.retain_sessions(|id, s| s.user != user_key || Some(id) == keep)
    }

    /// Drop sessions `is_live` rejects; returns how many went.
    pub fn retain_sessions(
        &self,
        mut is_live: impl FnMut(&str, &Session) -> bool,
    ) -> Result<usize> {
        let tx = self.db.begin_write()?;
        let mut removed = 0;
        {
            let mut t = tx.open_table(SESSIONS)?;
            t.retain(|id, bytes| {
                let keep = serde_json::from_slice::<Session>(bytes).is_ok_and(|s| is_live(id, &s));
                if !keep {
                    removed += 1;
                }
                keep
            })?;
        }
        tx.commit()?;
        Ok(removed)
    }

    // --- trash -----------------------------------------------------------------------

    pub fn put_trash(&self, item: &TrashItem) -> Result<()> {
        self.put_json(TRASH, &item.id, item, true)
    }

    pub fn trash_item(&self, id: &str) -> Result<Option<TrashItem>> {
        self.get_json(TRASH, id)
    }

    pub fn trash_items(&self) -> Result<Vec<TrashItem>> {
        self.all_json(TRASH)
    }

    pub fn delete_trash(&self, id: &str) -> Result<()> {
        self.delete_key(TRASH, id)
    }

    // --- uploads ---------------------------------------------------------------------

    /// `durable = false` for progress updates: losing one only means resending chunks.
    pub fn put_upload(&self, rec: &UploadRecord, durable: bool) -> Result<()> {
        self.put_json(UPLOADS, &rec.id, rec, durable)
    }

    pub fn upload(&self, id: &str) -> Result<Option<UploadRecord>> {
        self.get_json(UPLOADS, id)
    }

    pub fn uploads(&self) -> Result<Vec<UploadRecord>> {
        self.all_json(UPLOADS)
    }

    pub fn delete_upload(&self, id: &str) -> Result<()> {
        self.delete_key(UPLOADS, id)
    }

    pub fn put_batch(&self, rec: &BatchRecord) -> Result<()> {
        self.put_json(BATCHES, &rec.id, rec, true)
    }

    pub fn batch(&self, id: &str) -> Result<Option<BatchRecord>> {
        self.get_json(BATCHES, id)
    }

    pub fn batches(&self) -> Result<Vec<BatchRecord>> {
        self.all_json(BATCHES)
    }

    pub fn delete_batch(&self, id: &str) -> Result<()> {
        self.delete_key(BATCHES, id)
    }

    // --- audit -----------------------------------------------------------------------

    pub fn audit(&self, ev: &AuditEvent) -> Result<()> {
        let bytes = serde_json::to_vec(ev)?;
        // Unique, increasing keys even for events in the same nanosecond.
        let key = {
            let now = now_nanos();
            let mut prev = self.audit_seq.load(Ordering::Relaxed);
            loop {
                let next = now.max(prev + 1);
                match self.audit_seq.compare_exchange(
                    prev,
                    next,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break next,
                    Err(p) => prev = p,
                }
            }
        };
        let tx = self.db.begin_write()?;
        tx.open_table(AUDIT)?.insert(key, bytes.as_slice())?;
        tx.commit()?;
        Ok(())
    }

    /// Newest first, at most `limit`.
    pub fn recent_audit(&self, limit: usize) -> Result<Vec<AuditEvent>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(AUDIT)?;
        let mut out = Vec::new();
        for row in t.iter()?.rev().take(limit) {
            let (_, v) = row?;
            out.push(serde_json::from_slice(v.value())?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(name: &str) -> User {
        User {
            name: name.into(),
            role: "admin".into(),
            password_hash: "x".into(),
            must_change_password: false,
            disabled: false,
            created: 1,
            last_login: None,
            settings: UserSettings::default(),
            known_remote_ips: vec![],
        }
    }

    fn session(user: &str) -> Session {
        Session {
            user: user.into(),
            created: 1,
            last_seen: 1,
            csrf: "c".into(),
            ip: "::1".into(),
            remote: false,
        }
    }

    #[test]
    fn users_are_found_case_insensitively() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join(FILE_NAME)).unwrap();
        db.put_user(&user("Tibor")).unwrap();
        assert_eq!(db.user("tibor").unwrap().unwrap().name, "Tibor");
        assert_eq!(db.user("TIBOR").unwrap().unwrap().name, "Tibor");
        assert!(db.user("other").unwrap().is_none());
        assert_eq!(db.users().unwrap().len(), 1);
    }

    #[test]
    fn a_second_open_is_refused_and_data_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        let db = Db::open(&path).unwrap();
        db.put_user(&user("jana")).unwrap();
        assert!(matches!(Db::open(&path), Err(DbError::Locked)));
        drop(db);
        assert!(Db::open(&path).unwrap().user("jana").unwrap().is_some());
    }

    #[test]
    fn sessions_and_bulk_removal() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join(FILE_NAME)).unwrap();
        db.put_session("a", &session("tibor"), true).unwrap();
        db.put_session("b", &session("tibor"), false).unwrap();
        db.put_session("c", &session("jana"), true).unwrap();
        assert_eq!(db.delete_user_sessions("tibor", Some("a")).unwrap(), 1);
        assert!(db.session("a").unwrap().is_some() && db.session("b").unwrap().is_none());
        db.delete_session("c").unwrap();
        assert!(db.session("c").unwrap().is_none());
    }

    #[test]
    fn audit_keeps_order() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join(FILE_NAME)).unwrap();
        for i in 0..5 {
            db.audit(&AuditEvent {
                time: i,
                actor: "anon".into(),
                ip: "::1".into(),
                action: format!("a{i}"),
                target: String::new(),
                detail: String::new(),
            })
            .unwrap();
        }
        let recent: Vec<String> = db
            .recent_audit(3)
            .unwrap()
            .into_iter()
            .map(|e| e.action)
            .collect();
        assert_eq!(recent, ["a4", "a3", "a2"]);
    }
}
