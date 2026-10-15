//! Bounded, single-owner SQLite KV worker. No SQLite calls run on executor threads.
use aes::Aes128;
use aes::cipher::{Block, BlockDecrypt, BlockEncrypt, KeyInit};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::Value;
use std::path::Path;
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle, ThreadId};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{oneshot, watch};

/// Maximum serialized JSON bytes, also used for legacy plaintext rows.
pub const MAX_VALUE_BYTES: usize = 2 * 1024 * 1024;
/// AES block padding plus hex expansion for a maximum-size JSON value.
pub const MAX_STORED_VALUE_BYTES: usize = (MAX_VALUE_BYTES / 16 + 1) * 32;

pub type Result<T> = std::result::Result<T, StorageError>;

/// Errors contain no keys, values, paths, SQL, or upstream error strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageError {
    InvalidCapacity,
    QueueFull,
    Closed,
    SameThread,
    WorkerFailed,
    Database,
    Schema,
    InvalidCiphertext,
    InvalidJson,
    ValueTooLarge,
    InvalidTtl,
    Clock,
    UpdatePanicked,
}
impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidCapacity => "storage capacity must be positive",
            Self::QueueFull => "storage queue full",
            Self::Closed => "storage closed",
            Self::SameThread => "storage worker reentry prohibited",
            Self::WorkerFailed => "storage worker failed",
            Self::Database => "storage database operation failed",
            Self::Schema => "storage schema incompatible",
            Self::InvalidCiphertext => "storage ciphertext invalid",
            Self::InvalidJson => "storage JSON invalid",
            Self::ValueTooLarge => "storage value exceeds size limit",
            Self::InvalidTtl => "storage TTL out of range",
            Self::Clock => "storage clock unavailable",
            Self::UpdatePanicked => "storage update callback panicked",
        })
    }
}
impl std::error::Error for StorageError {}

// Public legacy protocol constant, NOT an application secret. AES-128 zero-padded key.
const KV_KEY: &[u8; 16] = b"LsPro-JxPan\0\0\0\0\0";

/// Encrypt an already serialized JSON string exactly as the frozen JS implementation.
/// Callers comparing ciphertext must use the same JSON.stringify byte representation.
pub fn encrypt_json_text(json: &str) -> Result<String> {
    if json.len() > MAX_VALUE_BYTES {
        return Err(StorageError::ValueTooLarge);
    }
    serde_json::from_str::<Value>(json).map_err(|_| StorageError::InvalidJson)?;
    let mut bytes = json.as_bytes().to_vec();
    let padding = 16 - bytes.len() % 16;
    bytes.resize(bytes.len() + padding, padding as u8);
    let cipher = Aes128::new_from_slice(KV_KEY).map_err(|_| StorageError::InvalidCiphertext)?;
    let mut hex = String::with_capacity(bytes.len() * 2);
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    for chunk in bytes.chunks_exact_mut(16) {
        let mut block = Block::<Aes128>::default();
        block.copy_from_slice(chunk);
        cipher.encrypt_block(&mut block);
        for byte in block {
            hex.push(DIGITS[(byte >> 4) as usize] as char);
            hex.push(DIGITS[(byte & 15) as usize] as char);
        }
    }
    Ok(hex)
}

pub fn encrypt_for_kv(value: &Value) -> Result<String> {
    // Serialize into a bounded buffer; do not first allocate an oversized string.
    struct JsonBuffer(Vec<u8>);
    impl std::io::Write for JsonBuffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > MAX_VALUE_BYTES - self.0.len() {
                return Err(std::io::Error::other("storage value exceeds size limit"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut buffer = JsonBuffer(Vec::new());
    serde_json::to_writer(&mut buffer, value).map_err(|_| StorageError::ValueTooLarge)?;
    let text = String::from_utf8(buffer.0).map_err(|_| StorageError::InvalidJson)?;
    encrypt_json_text(&text)
}

/// Valid legacy ciphertext is compatible; malformed hex/padding is rejected more strictly.
pub fn decrypt_from_kv(hex: &str) -> Result<Value> {
    if hex.len() > MAX_STORED_VALUE_BYTES {
        return Err(StorageError::ValueTooLarge);
    }
    if hex.is_empty() || !hex.len().is_multiple_of(32) {
        return Err(StorageError::InvalidCiphertext);
    }
    fn digit(b: u8) -> Result<u8> {
        match b {
            b'0'..=b'9' => Ok(b - b'0'),
            b'a'..=b'f' => Ok(b - b'a' + 10),
            b'A'..=b'F' => Ok(b - b'A' + 10),
            _ => Err(StorageError::InvalidCiphertext),
        }
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for pair in hex.as_bytes().chunks_exact(2) {
        bytes.push(digit(pair[0])? * 16 + digit(pair[1])?);
    }
    let cipher = Aes128::new_from_slice(KV_KEY).map_err(|_| StorageError::InvalidCiphertext)?;
    for chunk in bytes.chunks_exact_mut(16) {
        let mut block = Block::<Aes128>::default();
        block.copy_from_slice(chunk);
        cipher.decrypt_block(&mut block);
        chunk.copy_from_slice(&block);
    }
    let padding = *bytes.last().ok_or(StorageError::InvalidCiphertext)? as usize;
    if !(1..=16).contains(&padding)
        || !bytes[bytes.len() - padding..]
            .iter()
            .all(|b| *b as usize == padding)
    {
        return Err(StorageError::InvalidCiphertext);
    }
    bytes.truncate(bytes.len() - padding);
    if bytes.len() > MAX_VALUE_BYTES {
        return Err(StorageError::ValueTooLarge);
    }
    // JS TextDecoder is replacement-based; valid JSON payloads are UTF-8 already.
    let text = String::from_utf8(bytes).map_err(|_| StorageError::InvalidCiphertext)?;
    serde_json::from_str(&text).map_err(|_| StorageError::InvalidJson)
}

/// The action returned by a synchronous, non-blocking worker callback.
/// Keep preserves both value and expiry; Delete removes the row.
pub enum Update {
    Keep,
    Delete,
    Put {
        value: Value,
        ttl_seconds: Option<i64>,
    },
}

type Task = Box<dyn FnOnce(&mut Connection) + Send + 'static>;
struct Inner {
    sender: Mutex<Option<mpsc::SyncSender<Task>>>,
    join: Mutex<Option<JoinHandle<()>>>,
    done: watch::Receiver<bool>,
    worker_id: ThreadId,
}
impl Drop for Inner {
    fn drop(&mut self) {
        // Never join here: Drop may run on a Tokio executor thread. Disconnecting
        // drains accepted commands and terminates the owner thread; no Arc cycle.
        if let Ok(sender) = self.sender.get_mut() {
            sender.take();
        }
    }
}

#[derive(Clone)]
pub struct Storage {
    inner: Arc<Inner>,
}
impl Storage {
    /// Opens and validates the database on the dedicated thread. Capacity counts
    /// queued commands (there can also be one command currently executing).
    pub async fn open(path: impl AsRef<Path>, capacity: usize) -> Result<Self> {
        if capacity == 0 {
            return Err(StorageError::InvalidCapacity);
        }
        let path = path.as_ref().to_path_buf();
        let (sender, receiver) = mpsc::sync_channel::<Task>(capacity);
        let (ready_tx, ready_rx) = oneshot::channel();
        let (done_tx, done) = watch::channel(false);
        let join = thread::Builder::new()
            .name("jxpan-sqlite-kv".into())
            .spawn(move || {
                // Signal completion even on early initialization failure.
                struct Completion(tokio::sync::watch::Sender<bool>);
                impl Drop for Completion {
                    fn drop(&mut self) {
                        self.0.send_replace(true);
                    }
                }
                let _completion = Completion(done_tx);
                let opened = open_connection(&path);
                match opened {
                    Ok(mut connection) => {
                        if ready_tx.send(Ok(())).is_err() {
                            return;
                        }
                        while let Ok(task) = receiver.recv() {
                            task(&mut connection);
                        }
                        // Connection is dropped before the completion signal.
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                    }
                }
            })
            .map_err(|_| StorageError::WorkerFailed)?;
        let worker_id = join.thread().id();
        // Construct before awaiting: cancellation drops the sender too.
        let storage = Self {
            inner: Arc::new(Inner {
                sender: Mutex::new(Some(sender)),
                join: Mutex::new(Some(join)),
                done,
                worker_id,
            }),
        };
        ready_rx.await.map_err(|_| StorageError::WorkerFailed)??;
        Ok(storage)
    }

    async fn command<T, F>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        if thread::current().id() == self.inner.worker_id {
            return Err(StorageError::SameThread);
        }
        let (reply_tx, reply_rx) = oneshot::channel();
        let task = Box::new(move |connection: &mut Connection| {
            let result = operation(connection);
            let _ = reply_tx.send(result);
        });
        {
            let guard = self
                .inner
                .sender
                .lock()
                .map_err(|_| StorageError::WorkerFailed)?;
            let sender = guard.as_ref().ok_or(StorageError::Closed)?;
            sender.try_send(task).map_err(|error| match error {
                mpsc::TrySendError::Full(_) => StorageError::QueueFull,
                mpsc::TrySendError::Disconnected(_) => StorageError::Closed,
            })?;
        }
        reply_rx.await.map_err(|_| StorageError::WorkerFailed)?
    }

    /// Execute a repository operation on the existing bounded SQLite worker.
    /// The callback must be synchronous and must not re-enter this worker.
    pub async fn with_connection<T, F>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        self.command(operation).await
    }

    pub async fn get(&self, key: &str) -> Result<Option<Value>> {
        let key = key.to_owned();
        self.command(move |db| read_value(db, &key, now_seconds()?))
            .await
    }

    /// None or zero TTL never expires. Negative TTL is accepted as in the old
    /// source; expiry is checked with `expires_at > 0 && expires_at < now`.
    pub async fn put(&self, key: &str, value: Value, ttl_seconds: Option<i64>) -> Result<()> {
        let key = key.to_owned();
        self.command(move |db| write_value(db, &key, &value, ttl_seconds, now_seconds()?))
            .await
    }

    pub async fn delete(&self, key: &str) -> Result<()> {
        let key = key.to_owned();
        self.command(move |db| delete_value(db, &key)).await
    }

    /// Atomically compare decoded JSON and replace/delete. None means absent
    /// (including expired). JSON null is a present Value::Null, not absence.
    pub async fn compare_and_swap(
        &self,
        key: &str,
        expected: Option<Value>,
        replacement: Option<Value>,
        ttl_seconds: Option<i64>,
    ) -> Result<bool> {
        let key = key.to_owned();
        self.command(move |db| {
            let tx = db
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|_| StorageError::Database)?;
            let now = now_seconds()?;
            let current = read_value(&tx, &key, now)?;
            let matched = current == expected;
            if matched {
                match replacement {
                    Some(value) => write_value(&tx, &key, &value, ttl_seconds, now)?,
                    None => delete_value(&tx, &key)?,
                }
            }
            tx.commit().map_err(|_| StorageError::Database)?;
            Ok(matched)
        })
        .await
    }

    /// Callback runs once inside BEGIN IMMEDIATE on the SQLite owner thread.
    /// It MUST NOT block, perform network I/O, or call this store. Returns the
    /// resulting value. Callback error/panic rolls back the entire transaction.
    pub async fn update<F>(&self, key: &str, update: F) -> Result<Option<Value>>
    where
        F: FnOnce(Option<Value>) -> Result<Update> + Send + 'static,
    {
        let key = key.to_owned();
        self.command(move |db| {
            let tx = db
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|_| StorageError::Database)?;
            let now = now_seconds()?;
            let current = read_value(&tx, &key, now)?;
            let action =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| update(current.clone())))
                    .map_err(|_| StorageError::UpdatePanicked)??;
            let result = match action {
                Update::Keep => current,
                Update::Delete => {
                    delete_value(&tx, &key)?;
                    None
                }
                Update::Put { value, ttl_seconds } => {
                    write_value(&tx, &key, &value, ttl_seconds, now)?;
                    Some(value)
                }
            };
            tx.commit().map_err(|_| StorageError::Database)?;
            Ok(result)
        })
        .await
    }

    /// Closes all clones, drains previously accepted commands, and joins the
    /// owner thread off the executor. Concurrent close callers wait for exit.
    pub async fn close(&self) -> Result<()> {
        if thread::current().id() == self.inner.worker_id {
            return Err(StorageError::SameThread);
        }
        self.inner
            .sender
            .lock()
            .map_err(|_| StorageError::WorkerFailed)?
            .take();
        let join = self
            .inner
            .join
            .lock()
            .map_err(|_| StorageError::WorkerFailed)?
            .take();
        if let Some(join) = join {
            tokio::task::spawn_blocking(move || {
                join.join().map_err(|_| StorageError::WorkerFailed)
            })
            .await
            .map_err(|_| StorageError::WorkerFailed)??;
        } else {
            let mut done = self.inner.done.clone();
            while !*done.borrow_and_update() {
                done.changed()
                    .await
                    .map_err(|_| StorageError::WorkerFailed)?;
            }
        }
        Ok(())
    }
}

fn now_seconds() -> Result<i64> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| StorageError::Clock)?
        .as_secs();
    i64::try_from(seconds).map_err(|_| StorageError::Clock)
}
/// Reserve a new pathname before SQLite can create it with its default mode.
/// Existing files are left untouched, including their permission bits.
#[cfg(unix)]
fn prepare_database_file(path: &Path) -> Result<bool> {
    use std::fs::OpenOptions;
    use std::io::ErrorKind;
    use std::os::unix::fs::OpenOptionsExt;

    if path.as_os_str().is_empty() || path == Path::new(":memory:") {
        return Ok(false);
    }
    // SQLite also supports memory URIs. Do not create a literal URI-named file.
    // On-disk URI filenames are not accepted: their actual target can differ
    // from the pathname reserved below and bypass the creation mode.
    if let Some(uri) = path.to_str().and_then(|s| s.strip_prefix("file:")) {
        let (name, query) = uri.split_once('?').unwrap_or((uri, ""));
        let modes: Vec<_> = url::form_urlencoded::parse(query.as_bytes())
            .filter(|(key, _)| key == "mode")
            .map(|(_, value)| value)
            .collect();
        if (name == ":memory:" || !modes.is_empty()) && modes.iter().all(|mode| mode == "memory") {
            return Ok(false);
        }
        return Err(StorageError::Database);
    }
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(true),
        Err(_) => Err(StorageError::Database),
    }
}

fn open_connection(path: &Path) -> Result<Connection> {
    #[cfg(unix)]
    let connection = {
        let on_disk = prepare_database_file(path)?;
        let mut flags = rusqlite::OpenFlags::default();
        if on_disk {
            // If the reserved file disappears, fail rather than let SQLite
            // recreate it with default permissions. Use a trusted parent dir.
            flags.remove(rusqlite::OpenFlags::SQLITE_OPEN_CREATE);
        }
        Connection::open_with_flags(path, flags).map_err(|_| StorageError::Database)?
    };
    #[cfg(not(unix))]
    let connection = Connection::open(path).map_err(|_| StorageError::Database)?;
    connection
        .busy_timeout(Duration::from_millis(250))
        .map_err(|_| StorageError::Database)?;
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS kv_store (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL,
            expires_at INTEGER DEFAULT 0,
            created_at INTEGER DEFAULT (strftime('%s', 'now'))
        );",
        )
        .map_err(|_| StorageError::Schema)?;
    // CREATE IF NOT EXISTS is not a migration: inspect a preexisting table.
    let mut statement = connection
        .prepare("PRAGMA table_info(kv_store)")
        .map_err(|_| StorageError::Schema)?;
    let columns = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(5)?,
            ))
        })
        .map_err(|_| StorageError::Schema)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| StorageError::Schema)?;
    if columns.iter().filter(|(_, _, _, pk)| *pk != 0).count() != 1 {
        return Err(StorageError::Schema);
    }
    let required = [
        ("key", "TEXT", false, true),
        ("value", "TEXT", true, false),
        ("expires_at", "INTEGER", false, false),
        ("created_at", "INTEGER", false, false),
    ];
    for (name, kind, not_null, primary) in required {
        if !columns.iter().any(|(n, k, nn, pk)| {
            n == name
                && k.eq_ignore_ascii_case(kind)
                && (!not_null || *nn != 0)
                && ((*pk != 0) == primary)
        }) {
            return Err(StorageError::Schema);
        }
    }
    drop(statement);
    Ok(connection)
}
fn delete_value(db: &Connection, key: &str) -> Result<()> {
    db.execute("DELETE FROM kv_store WHERE key = ?1", [key])
        .map_err(|_| StorageError::Database)?;
    Ok(())
}
fn read_value(db: &Connection, key: &str, now: i64) -> Result<Option<Value>> {
    let row: Option<(Option<i64>, Option<String>)> = db
        .query_row(
            // SQLite byte length (not text character count) gates the projection
            // before rusqlite can copy the field into a Rust String. Plain JSON
            // and padded hex ciphertext have the same limits as the writer.
            "SELECT expires_at, CASE WHEN length(CAST(value AS BLOB)) <=
             CASE WHEN substr(value, 1, 1) IN ('{', '[') THEN ?2 ELSE ?3 END
             THEN value ELSE NULL END FROM kv_store WHERE key = ?1",
            params![key, MAX_VALUE_BYTES, MAX_STORED_VALUE_BYTES],
            |row| {
                let expires_at: Option<i64> = row.get(0)?;
                // Preserve legacy expiry cleanup without materializing the value.
                let raw = if expires_at.is_some_and(|expiry| expiry > 0 && expiry < now) {
                    None
                } else {
                    row.get(1)?
                };
                Ok((expires_at, raw))
            },
        )
        .optional()
        .map_err(|_| StorageError::Database)?;
    let Some((expires_at, raw)) = row else {
        return Ok(None);
    };
    if expires_at.is_some_and(|expiry| expiry > 0 && expiry < now) {
        delete_value(db, key)?;
        return Ok(None);
    }
    let raw = raw.ok_or(StorageError::ValueTooLarge)?;
    if raw.is_empty() {
        return Ok(None);
    }
    let value = if raw.starts_with('{') || raw.starts_with('[') {
        serde_json::from_str(&raw).map_err(|_| StorageError::InvalidJson)?
    } else {
        decrypt_from_kv(&raw)?
    };
    Ok(Some(value))
}
fn write_value(
    db: &Connection,
    key: &str,
    value: &Value,
    ttl: Option<i64>,
    now: i64,
) -> Result<()> {
    let expiry = match ttl {
        None | Some(0) => 0,
        Some(ttl) => now.checked_add(ttl).ok_or(StorageError::InvalidTtl)?,
    };
    let encrypted = encrypt_for_kv(value)?;
    db.execute(
        "INSERT OR REPLACE INTO kv_store (key, value, expires_at) VALUES (?1, ?2, ?3)",
        params![key, encrypted, expiry],
    )
    .map_err(|_| StorageError::Database)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[cfg(unix)]
    #[test]
    fn reservation_is_private_before_sqlite_initializes() {
        use std::os::unix::fs::PermissionsExt;

        struct ReservedFile(std::path::PathBuf);
        impl Drop for ReservedFile {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let file = ReservedFile(std::env::temp_dir().join(format!(
            "jxpan-storage-reserved-{}.sqlite",
            uuid::Uuid::new_v4()
        )));
        assert!(prepare_database_file(&file.0).unwrap());
        let metadata = std::fs::metadata(&file.0).unwrap();
        assert_eq!(metadata.len(), 0); // SQLite has not opened or written it yet.
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        std::fs::set_permissions(&file.0, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert!(prepare_database_file(&file.0).unwrap());
        assert_eq!(
            std::fs::metadata(&file.0).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }

    #[test]
    fn exact_expiry_boundary_and_cleanup() {
        let db = open_connection(Path::new(":memory:")).unwrap();
        write_value(&db, "k", &json!({"offline": true}), Some(10), 100).unwrap();
        assert!(read_value(&db, "k", 110).unwrap().is_some());
        assert_eq!(read_value(&db, "k", 111).unwrap(), None);
        let count: i64 = db
            .query_row("SELECT count(*) FROM kv_store", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
        for ttl in [None, Some(0), Some(-100), Some(-101)] {
            write_value(&db, "k", &json!(null), ttl, 100).unwrap();
            assert_eq!(read_value(&db, "k", 1000).unwrap(), Some(Value::Null));
        }
        assert_eq!(
            write_value(&db, "k", &json!(1), Some(i64::MAX), 1),
            Err(StorageError::InvalidTtl)
        );
    }

    #[test]
    fn frozen_expiry_truth_table() {
        let fixture: Value =
            serde_json::from_str(include_str!("../tests/fixtures/storage.json")).unwrap();
        let db = open_connection(Path::new(":memory:")).unwrap();
        for vector in fixture["expiry"].as_array().unwrap() {
            let expiry = vector["expires_at"].as_i64();
            let now = vector["now"].as_i64().unwrap();
            db.execute(
                "INSERT OR REPLACE INTO kv_store (key,value,expires_at) VALUES ('k',?1,?2)",
                params![encrypt_for_kv(&json!({"offline": true})).unwrap(), expiry],
            )
            .unwrap();
            assert_eq!(
                read_value(&db, "k", now).unwrap().is_some(),
                vector["present"].as_bool().unwrap()
            );
            let count: i64 = db
                .query_row("SELECT count(*) FROM kv_store WHERE key='k'", [], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(count == 0, vector["deleted"].as_bool().unwrap());
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn last_handle_drop_terminates_worker() {
        let store = Storage::open(":memory:", 2).await.unwrap();
        let clone = store.clone();
        let mut done = store.inner.done.clone();
        drop(store);
        assert!(!*done.borrow());
        clone.put("synthetic", json!(1), None).await.unwrap();
        drop(clone);
        while !*done.borrow_and_update() {
            done.changed().await.unwrap();
        }
    }

    #[test]
    fn ciphertext_within_hex_limit_cannot_exceed_json_byte_limit() {
        let value = json!("x".repeat(MAX_VALUE_BYTES - 2));
        let mut encrypted = encrypt_for_kv(&value).unwrap();
        assert_eq!(encrypted.len(), MAX_STORED_VALUE_BYTES);
        // The boundary value has a full padding block. Replace that block with
        // one JSON whitespace byte plus valid padding: hex length stays the same
        // but decoded serialized JSON is now one byte over the writer's limit.
        let mut block = Block::<Aes128>::default();
        block.fill(15);
        block[0] = b' ';
        Aes128::new_from_slice(KV_KEY)
            .unwrap()
            .encrypt_block(&mut block);
        encrypted.truncate(encrypted.len() - 32);
        for byte in block {
            use std::fmt::Write;
            write!(&mut encrypted, "{byte:02x}").unwrap();
        }
        assert_eq!(
            decrypt_from_kv(&encrypted),
            Err(StorageError::ValueTooLarge)
        );
    }

    #[test]
    fn rejects_invalid_ciphertext_without_echoing_input() {
        for text in [
            "",
            "offline-secret",
            "f",
            "gggggggggggggggggggggggggggggggg",
            "00000000000000000000000000000000",
        ] {
            assert!(decrypt_from_kv(text).is_err());
        }
    }
}
