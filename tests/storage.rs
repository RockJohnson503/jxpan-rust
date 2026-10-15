mod support;
use jxpan_rust::storage;

use futures_util::{FutureExt, poll};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::mpsc;
use storage::{
    MAX_STORED_VALUE_BYTES, MAX_VALUE_BYTES, Storage, StorageError, Update, decrypt_from_kv,
    encrypt_for_kv, encrypt_json_text,
};

struct TempDb(PathBuf);
impl TempDb {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("jxpan-storage-{}.sqlite", uuid::Uuid::new_v4())))
    }
}
impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
fn fixture() -> Value {
    support::oracle("storage", include_str!("fixtures/storage.json"))
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn new_database_is_owner_only_and_stays_so_after_reopen() {
    use std::os::unix::fs::PermissionsExt;

    let temp = TempDb::new();
    assert!(!temp.0.exists());
    let store = Storage::open(&temp.0, 4).await.unwrap();
    assert_eq!(
        std::fs::metadata(&temp.0).unwrap().permissions().mode() & 0o777,
        0o600
    );
    store.put("synthetic", json!(1), None).await.unwrap();
    store.close().await.unwrap();
    let reopened = Storage::open(&temp.0, 4).await.unwrap();
    assert_eq!(reopened.get("synthetic").await.unwrap(), Some(json!(1)));
    assert_eq!(
        std::fs::metadata(&temp.0).unwrap().permissions().mode() & 0o777,
        0o600
    );
    reopened.close().await.unwrap();
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn existing_database_permissions_are_not_changed() {
    use std::os::unix::fs::PermissionsExt;

    for mode in [0o600, 0o640, 0o644, 0o660] {
        let temp = TempDb::new();
        let initial = Storage::open(&temp.0, 4).await.unwrap();
        initial.put("synthetic", json!(1), None).await.unwrap();
        initial.close().await.unwrap();
        std::fs::set_permissions(&temp.0, std::fs::Permissions::from_mode(mode)).unwrap();
        let store = Storage::open(&temp.0, 4).await.unwrap();
        assert_eq!(store.get("synthetic").await.unwrap(), Some(json!(1)));
        store.put("synthetic", json!(2), None).await.unwrap();
        store.close().await.unwrap();
        assert_eq!(
            std::fs::metadata(&temp.0).unwrap().permissions().mode() & 0o777,
            mode
        );
    }
    // Even an existing empty file must not be mistaken for a new file.
    let temp = TempDb::new();
    std::fs::File::create(&temp.0).unwrap();
    std::fs::set_permissions(&temp.0, std::fs::Permissions::from_mode(0o644)).unwrap();
    let store = Storage::open(&temp.0, 4).await.unwrap();
    store.close().await.unwrap();
    assert_eq!(
        std::fs::metadata(&temp.0).unwrap().permissions().mode() & 0o777,
        0o644
    );
}

#[tokio::test(flavor = "current_thread")]
async fn memory_and_temporary_databases_remain_ephemeral() {
    let named = format!(
        "file:jxpan-memory-{}?mode=memory&cache=shared",
        uuid::Uuid::new_v4()
    );
    for path in [":memory:", "", "file::memory:", named.as_str()] {
        let store = Storage::open(path, 4).await.unwrap();
        store
            .put("synthetic", json!({"memory": true}), None)
            .await
            .unwrap();
        assert_eq!(
            store.get("synthetic").await.unwrap(),
            Some(json!({"memory": true}))
        );
        store.close().await.unwrap();
        let reopened = Storage::open(path, 4).await.unwrap();
        assert_eq!(reopened.get("synthetic").await.unwrap(), None);
        reopened.close().await.unwrap();
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn on_disk_uri_cannot_bypass_private_creation() {
    let temp = TempDb::new();
    let uri = format!("file:{}?mode=rwc", temp.0.display());
    assert!(matches!(
        Storage::open(&uri, 4).await,
        Err(StorageError::Database)
    ));
    assert!(!temp.0.exists());
}

#[test]
fn exact_frozen_aes_kv_vectors() {
    let fixture = fixture();
    assert_eq!(
        fixture["source_sha256"],
        "47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118"
    );
    for vector in fixture["vectors"].as_array().unwrap() {
        let hex = vector["ciphertext"].as_str().unwrap();
        let json_text = vector["json"].as_str().unwrap();
        assert_eq!(
            encrypt_json_text(json_text).unwrap(),
            hex,
            "fixture text encryption mismatch"
        );
        assert_eq!(
            encrypt_for_kv(&vector["value"]).unwrap(),
            hex,
            "fixture value encryption mismatch"
        );
        assert_eq!(
            decrypt_from_kv(hex).unwrap(),
            vector["value"],
            "fixture decryption mismatch"
        );
        assert_eq!(
            decrypt_from_kv(&hex.to_uppercase()).unwrap(),
            vector["value"]
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn roundtrip_persistence_delete_and_legacy_plain_json() {
    let temp = TempDb::new();
    let store = Storage::open(&temp.0, 4).await.unwrap();
    assert_eq!(store.get("missing").await.unwrap(), None);
    for value in [
        json!({"offline": "离线 🧪"}),
        json!([1, 2]),
        json!(null),
        json!("text"),
        json!(true),
        json!(42),
    ] {
        store.put("key", value.clone(), None).await.unwrap();
        assert_eq!(store.get("key").await.unwrap(), Some(value));
    }
    store.put("saved", json!({"a": 1}), Some(0)).await.unwrap();
    store.delete("key").await.unwrap();
    store.delete("missing").await.unwrap();
    assert_eq!(store.get("key").await.unwrap(), None);
    store.close().await.unwrap();
    assert_eq!(store.get("saved").await, Err(StorageError::Closed));
    // Direct SQLite fixture setup only after worker termination; no real data.
    {
        let db = Connection::open(&temp.0).unwrap();
        let stored: String = db
            .query_row("SELECT value FROM kv_store WHERE key='saved'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(stored, encrypt_for_kv(&json!({"a": 1})).unwrap());
        db.execute(
            "INSERT INTO kv_store (key,value) VALUES ('plain',?1)",
            [r#"{"legacy":[1,2]}"#],
        )
        .unwrap();
        db.execute(
            "INSERT INTO kv_store (key,value) VALUES ('bad',?1)",
            ["offline-not-ciphertext"],
        )
        .unwrap();
        db.execute("INSERT INTO kv_store (key,value) VALUES ('empty','')", [])
            .unwrap();
    }
    let reopened = Storage::open(&temp.0, 4).await.unwrap();
    assert_eq!(reopened.get("saved").await.unwrap(), Some(json!({"a": 1})));
    assert_eq!(
        reopened.get("plain").await.unwrap(),
        Some(json!({"legacy": [1, 2]}))
    );
    assert_eq!(
        reopened.get("bad").await,
        Err(StorageError::InvalidCiphertext)
    );
    assert_eq!(reopened.get("empty").await.unwrap(), None);
    reopened.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn kv_size_boundaries_match_json_bytes_and_ciphertext_expansion() {
    let store = Storage::open(":memory:", 16).await.unwrap();
    let value = json!("界".repeat((MAX_VALUE_BYTES - 2) / 3));
    let text = serde_json::to_string(&value).unwrap();
    assert_eq!(text.len(), MAX_VALUE_BYTES);
    assert!(text.chars().count() < MAX_VALUE_BYTES);
    let encrypted = encrypt_for_kv(&value).unwrap();
    assert_eq!(encrypted.len(), MAX_STORED_VALUE_BYTES);
    assert_eq!(decrypt_from_kv(&encrypted).unwrap(), value);
    store.put("boundary", value.clone(), None).await.unwrap();
    assert_eq!(store.get("boundary").await.unwrap(), Some(value));

    // Plain legacy JSON uses the decoded JSON budget, not the larger hex budget.
    let prefix = r#"{"data":""#;
    let suffix = r#""}"#;
    let plain = format!(
        "{prefix}{}{suffix}",
        "x".repeat(MAX_VALUE_BYTES - prefix.len() - suffix.len())
    );
    assert_eq!(plain.len(), MAX_VALUE_BYTES);
    let expected: Value = serde_json::from_str(&plain).unwrap();
    store
        .with_connection(move |db| {
            db.execute(
                "INSERT INTO kv_store (key, value) VALUES ('plain-boundary', ?1)",
                [plain],
            )
            .map(|_| ())
            .map_err(|_| StorageError::Database)
        })
        .await
        .unwrap();
    assert_eq!(store.get("plain-boundary").await.unwrap(), Some(expected));
    store.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn oversized_reads_are_gated_before_row_get_and_preserve_existing_rows() {
    let store = Storage::open(":memory:", 16).await.unwrap();
    let unicode =
        serde_json::to_string(&json!({"data":"界".repeat(MAX_VALUE_BYTES / 3 + 1)})).unwrap();
    assert!(unicode.len() > MAX_VALUE_BYTES);
    assert!(unicode.chars().count() < MAX_VALUE_BYTES);
    let nul = format!("{{\0{}", "x".repeat(MAX_VALUE_BYTES));
    store
        .with_connection(move |db| {
            db.execute(
                "INSERT INTO kv_store (key, value) VALUES ('unicode', ?1), ('nul', ?2)",
                params![unicode, nul],
            )
            .map_err(|_| StorageError::Database)?;
            // A BLOB cannot be row.get::<String>'d. Returning ValueTooLarge rather
            // than Database proves that SQL withheld the oversized field first.
            db.execute(
                "INSERT INTO kv_store (key, value) VALUES ('oversized-blob', zeroblob(?1)),
             ('oversized-text', CAST(zeroblob(?1) AS TEXT))",
                [MAX_STORED_VALUE_BYTES + 1],
            )
            .map(|_| ())
            .map_err(|_| StorageError::Database)
        })
        .await
        .unwrap();
    for key in ["unicode", "nul", "oversized-blob", "oversized-text"] {
        assert_eq!(
            store.get(key).await,
            Err(StorageError::ValueTooLarge),
            "{key}"
        );
        assert_eq!(
            store
                .compare_and_swap(key, None, Some(json!(1)), None)
                .await,
            Err(StorageError::ValueTooLarge)
        );
        assert_eq!(
            store
                .update(key, |_| panic!(
                    "oversized input must not reach the callback"
                ))
                .await,
            Err(StorageError::ValueTooLarge)
        );
    }
    let count = store
        .with_connection(|db| {
            db.query_row("SELECT count(*) FROM kv_store", [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(|_| StorageError::Database)
        })
        .await
        .unwrap();
    assert_eq!(count, 4);
    assert_eq!(
        StorageError::ValueTooLarge.to_string(),
        "storage value exceeds size limit"
    );
    store.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn oversized_writes_fail_before_replacement_and_roll_back_atomic_updates() {
    let store = Storage::open(":memory:", 16).await.unwrap();
    let old = json!({"synthetic":true});
    store.put("existing", old.clone(), None).await.unwrap();
    let value = json!("x".repeat(MAX_VALUE_BYTES - 1));
    assert_eq!(encrypt_for_kv(&value), Err(StorageError::ValueTooLarge));
    assert_eq!(
        encrypt_json_text(&serde_json::to_string(&value).unwrap()),
        Err(StorageError::ValueTooLarge)
    );
    assert_eq!(
        store.put("existing", value.clone(), None).await,
        Err(StorageError::ValueTooLarge)
    );
    assert_eq!(
        store
            .compare_and_swap("existing", Some(old.clone()), Some(value.clone()), None)
            .await,
        Err(StorageError::ValueTooLarge)
    );
    assert_eq!(
        store
            .update("existing", move |_| Ok(Update::Put {
                value,
                ttl_seconds: None
            }))
            .await,
        Err(StorageError::ValueTooLarge)
    );
    assert_eq!(store.get("existing").await.unwrap(), Some(old));
    // JSON escapes count too: the source string itself fits, its serialization does not.
    assert_eq!(
        store
            .put("escaped", json!("\0".repeat(MAX_VALUE_BYTES / 6 + 1)), None)
            .await,
        Err(StorageError::ValueTooLarge)
    );
    assert_eq!(store.get("escaped").await.unwrap(), None);
    store.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn oversized_expired_value_keeps_legacy_cleanup_semantics() {
    let store = Storage::open(":memory:", 16).await.unwrap();
    store
        .with_connection(|db| {
            db.execute(
                "INSERT INTO kv_store (key, value, expires_at) VALUES
             ('expired', zeroblob(?1), 1), ('not-expired', zeroblob(?1), 0)",
                [MAX_STORED_VALUE_BYTES + 1],
            )
            .map(|_| ())
            .map_err(|_| StorageError::Database)
        })
        .await
        .unwrap();
    assert_eq!(store.get("expired").await.unwrap(), None);
    assert_eq!(
        store.get("not-expired").await,
        Err(StorageError::ValueTooLarge)
    );
    let count = store
        .with_connection(|db| {
            db.query_row("SELECT count(*) FROM kv_store", [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(|_| StorageError::Database)
        })
        .await
        .unwrap();
    assert_eq!(count, 1);
    store.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn expiry_cleanup_zero_negative_and_overflow() {
    let temp = TempDb::new();
    let store = Storage::open(&temp.0, 4).await.unwrap();
    store.put("expired", json!(1), Some(-1)).await.unwrap();
    assert_eq!(store.get("expired").await.unwrap(), None);
    store.put("no-expiry", json!(1), Some(0)).await.unwrap();
    assert_eq!(store.get("no-expiry").await.unwrap(), Some(json!(1)));
    assert_eq!(
        store.put("overflow", json!(1), Some(i64::MAX)).await,
        Err(StorageError::InvalidTtl)
    );
    store.close().await.unwrap();
    let db = Connection::open(&temp.0).unwrap();
    let count: i64 = db
        .query_row(
            "SELECT count(*) FROM kv_store WHERE key='expired' OR key='overflow'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn compare_swap_and_atomic_updates_are_serialized() {
    let temp = TempDb::new();
    let store = Storage::open(&temp.0, 32).await.unwrap();
    assert!(
        store
            .compare_and_swap("cas", None, Some(Value::Null), None)
            .await
            .unwrap()
    );
    assert!(
        !store
            .compare_and_swap("cas", None, Some(json!(1)), None)
            .await
            .unwrap()
    );
    assert!(
        store
            .compare_and_swap("cas", Some(Value::Null), Some(json!(1)), None)
            .await
            .unwrap()
    );
    assert!(
        !store
            .compare_and_swap("cas", Some(json!(0)), None, None)
            .await
            .unwrap()
    );
    assert!(
        store
            .compare_and_swap("cas", Some(json!(1)), None, None)
            .await
            .unwrap()
    );
    store.put("cas", json!(2), Some(-1)).await.unwrap();
    assert!(
        store
            .compare_and_swap("cas", None, Some(json!(3)), None)
            .await
            .unwrap()
    );
    let mut handles = Vec::new();
    for _ in 0..8 {
        let cloned = store.clone();
        handles.push(tokio::spawn(async move {
            for _ in 0..40 {
                cloned
                    .update("counter", |current| {
                        Ok(Update::Put {
                            value: json!(current.and_then(|v| v.as_u64()).unwrap_or(0) + 1),
                            ttl_seconds: None,
                        })
                    })
                    .await
                    .unwrap();
            }
        }));
    }
    for handle in handles {
        handle.await.unwrap();
    }
    assert_eq!(store.get("counter").await.unwrap(), Some(json!(320)));
    assert_eq!(
        store
            .update("counter", |_| Err(StorageError::InvalidTtl))
            .await,
        Err(StorageError::InvalidTtl)
    );
    assert_eq!(store.get("counter").await.unwrap(), Some(json!(320)));
    assert_eq!(
        store.update("counter", |_| Ok(Update::Keep)).await.unwrap(),
        Some(json!(320))
    );
    assert_eq!(
        store
            .update("counter", |_| Ok(Update::Delete))
            .await
            .unwrap(),
        None
    );
    store.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn queue_full_is_immediate_and_executor_keeps_running() {
    let temp = TempDb::new();
    let store = Storage::open(&temp.0, 1).await.unwrap();
    let worker = store.clone();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = mpsc::channel();
    // Intentionally stall the owner in this test only, never the Tokio thread.
    let active = tokio::spawn(async move {
        worker
            .update("active", move |_| {
                let _ = started_tx.send(());
                let _ = release_rx.recv();
                Ok(Update::Keep)
            })
            .await
    });
    started_rx.await.unwrap();
    let mut queued = Box::pin(store.get("queued"));
    assert!(poll!(queued.as_mut()).is_pending());
    assert_eq!(
        store.put("rejected", json!("synthetic"), None).await,
        Err(StorageError::QueueFull)
    );
    let mut ticks = 0;
    for _ in 0..10 {
        tokio::task::yield_now().await;
        ticks += 1;
    }
    assert_eq!(ticks, 10);
    release_tx.send(()).unwrap();
    assert_eq!(active.await.unwrap().unwrap(), None);
    assert_eq!(queued.await.unwrap(), None);
    assert_eq!(store.get("rejected").await.unwrap(), None);
    store.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn reentry_rejected_and_callback_panic_rolls_back() {
    let temp = TempDb::new();
    let store = Storage::open(&temp.0, 4).await.unwrap();
    store
        .put("key", json!({"synthetic": 1}), None)
        .await
        .unwrap();
    let inside = store.clone();
    store
        .update("key", move |_| {
            assert_eq!(
                inside.get("key").now_or_never(),
                Some(Err(StorageError::SameThread))
            );
            assert_eq!(
                inside.close().now_or_never(),
                Some(Err(StorageError::SameThread))
            );
            Ok(Update::Keep)
        })
        .await
        .unwrap();
    assert_eq!(
        store
            .update("key", |_| panic!("synthetic test panic"))
            .await,
        Err(StorageError::UpdatePanicked)
    );
    assert_eq!(
        store.get("key").await.unwrap(),
        Some(json!({"synthetic": 1}))
    );
    store.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn schema_mismatch_and_open_failure_are_static_errors() {
    let temp = TempDb::new();
    let db = Connection::open(&temp.0).unwrap();
    db.execute(
        "CREATE TABLE kv_store(key TEXT,value TEXT,expiration INTEGER)",
        [],
    )
    .unwrap();
    drop(db);
    let result = Storage::open(&temp.0, 4).await;
    assert!(matches!(result, Err(StorageError::Schema)));
    assert!(matches!(
        Storage::open(&temp.0, 0).await,
        Err(StorageError::InvalidCapacity)
    ));
    let missing = temp.0.join("missing-parent.sqlite");
    assert!(matches!(
        Storage::open(&missing, 4).await,
        Err(StorageError::Database)
    ));
    assert_eq!(
        StorageError::Database.to_string(),
        "storage database operation failed"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn explicit_close_drains_and_closes_all_clones() {
    let temp = TempDb::new();
    let store = Storage::open(&temp.0, 4).await.unwrap();
    let cloned = store.clone();
    let mut write = Box::pin(store.put("accepted", json!(1), None));
    assert!(poll!(write.as_mut()).is_pending());
    let (a, b) = tokio::join!(store.close(), cloned.close());
    a.unwrap();
    b.unwrap();
    write.await.unwrap();
    assert_eq!(cloned.delete("accepted").await, Err(StorageError::Closed));
    // The joined thread released the connection; reopen locally and verify drain.
    let db = Connection::open(&temp.0).unwrap();
    let raw: String = db
        .query_row(
            "SELECT value FROM kv_store WHERE key=?1",
            params!["accepted"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(decrypt_from_kv(&raw).unwrap(), json!(1));
}
