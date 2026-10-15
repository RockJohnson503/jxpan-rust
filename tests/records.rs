#[tokio::test(flavor = "current_thread")]
async fn old_redacted_payload_is_marked_incomplete_not_silently_restored() {
    let temp = TempDb::new();
    let store = temp.open().await;
    let records = Records::new_at(store.clone(), NOW).await.unwrap();
    records
        .insert_at("https://example.invalid", "", &success(), false, NOW)
        .await
        .unwrap();
    store
        .with_connection(|db| {
            let legacy =
                storage::encrypt_for_kv(&json!({"url":"https://example.invalid/[redacted]",
            "pwd":"[redacted]", "code":200, "msg":"解析成功", "data":null}))?;
            db.execute("UPDATE parse_records SET payload = ?1", [legacy])
                .map_err(|_| StorageError::Database)?;
            Ok(())
        })
        .await
        .unwrap();
    let row = records.list_at(1, 1, "", "", NOW).await.unwrap()["success"][0].clone();
    assert_eq!(row["truncated"], true);
    assert_eq!(row["pwd"], "[redacted]");
    store.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn frozen_request_counter_events_remain_independent_of_history() {
    let fixture = support::oracle("records", include_str!("fixtures/records.json"));
    let temp = TempDb::new();
    let store = temp.open().await;
    let records = Records::new_at(store.clone(), NOW).await.unwrap();
    for event in fixture["counter_events"].as_array().unwrap() {
        records
            .insert_at(
                "https://example.invalid/share",
                "synthetic-share",
                &event["result"],
                event["cached"].as_bool().unwrap(),
                NOW,
            )
            .await
            .unwrap();
        assert_eq!(records.stats_at(NOW).await.unwrap(), event["stats"]);
    }
    let list = records.list_at(1, 10, "", "", NOW).await.unwrap();
    assert_eq!(list["successTotal"], 2);
    assert_eq!(list["failedTotal"], 2);
    records.clear().await.unwrap();
    assert_eq!(
        records.stats_at(NOW).await.unwrap(),
        fixture["counter_events"][3]["stats"]
    );
    store.close().await.unwrap();
}

mod support;
use jxpan_rust::{records, storage};

use records::Records;
use serde_json::{Value, json};
use std::path::PathBuf;
use storage::{Storage, StorageError};

const DAY: i64 = 86_400_000;
const NOW: i64 = 1_700_000_000_000;

struct TempDb(PathBuf);
impl TempDb {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("jxpan-records-{}.sqlite", uuid::Uuid::new_v4())))
    }
    async fn open(&self) -> Storage {
        Storage::open(&self.0, 256).await.unwrap()
    }
}
impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
fn success() -> Value {
    json!({"success": true, "code": 200, "msg": "解析成功"})
}

#[tokio::test(flavor = "current_thread")]
async fn default_boundary_and_cleanup_leave_other_data_untouched() {
    let temp = TempDb::new();
    let store = temp.open().await;
    for key in [
        "credential_synthetic",
        "qr_synthetic",
        "short_synthetic",
        "plain_kv",
        "parse_record_legacy",
    ] {
        store
            .put(key, json!({"synthetic": true}), None)
            .await
            .unwrap();
    }
    let records = Records::new_at(store.clone(), NOW - 7 * DAY - 1)
        .await
        .unwrap();
    assert_eq!(records.retention_days().await.unwrap(), 7);
    for time in [NOW - 7 * DAY - 1, NOW - 7 * DAY, NOW - 7 * DAY + 1] {
        records
            .insert_at("https://example.invalid/share", "", &success(), false, time)
            .await
            .unwrap();
    }
    assert_eq!(records.stats_at(NOW).await.unwrap()["total"], 3);
    assert_eq!(records.cleanup_at(NOW).await.unwrap(), 1);
    assert_eq!(records.cleanup_at(NOW).await.unwrap(), 0);
    let list = records.list_at(1, 10, "", "", NOW).await.unwrap();
    assert_eq!(list["total"], 2);
    assert_eq!(list["success"][1]["timestamp"], "2023-11-07T22:13:20.000Z");
    for key in [
        "credential_synthetic",
        "qr_synthetic",
        "short_synthetic",
        "plain_kv",
        "parse_record_legacy",
    ] {
        assert_eq!(
            store.get(key).await.unwrap(),
            Some(json!({"synthetic": true}))
        );
    }
    // The cleanup predicate has a usable index, independent of total payload size.
    store
        .with_connection(|db| {
            let mut plan = db
                .prepare("EXPLAIN QUERY PLAN DELETE FROM parse_records WHERE created_ms < ?1")
                .map_err(|_| StorageError::Database)?;
            let details = plan
                .query_map([NOW], |row| row.get::<_, String>(3))
                .map_err(|_| StorageError::Database)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|_| StorageError::Database)?;
            assert!(
                details
                    .iter()
                    .any(|line| line.contains("parse_records_created_ms"))
            );
            Ok(())
        })
        .await
        .unwrap();
    store.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn settings_persist_decrease_immediately_and_increase_cannot_restore() {
    let temp = TempDb::new();
    let store = temp.open().await;
    let records = Records::new_at(store.clone(), NOW - 10 * DAY)
        .await
        .unwrap();
    records
        .set_retention_days_at(30, NOW - 10 * DAY)
        .await
        .unwrap();
    for time in [NOW - 10 * DAY, NOW - 2 * DAY - 1, NOW - 2 * DAY, NOW] {
        records
            .insert_at("https://example.invalid", "", &success(), false, time)
            .await
            .unwrap();
    }
    assert_eq!(records.stats_at(NOW).await.unwrap()["total"], 4);
    records.set_retention_days_at(2, NOW).await.unwrap();
    assert_eq!(records.stats_at(NOW).await.unwrap()["total"], 4);
    assert_eq!(records.count().await.unwrap(), 2);
    assert_eq!(records.cleanup_at(NOW).await.unwrap(), 0);
    records.set_retention_days_at(3650, NOW).await.unwrap();
    assert_eq!(records.stats_at(NOW).await.unwrap()["total"], 4);
    assert_eq!(records.count().await.unwrap(), 2);
    for days in [0, 3651, u16::MAX] {
        assert_eq!(
            records.set_retention_days_at(days, NOW).await,
            Err(StorageError::InvalidTtl)
        );
        assert_eq!(records.retention_days().await.unwrap(), 3650);
    }
    records.set_retention_days_at(1, NOW).await.unwrap();
    assert_eq!(records.stats_at(NOW).await.unwrap()["total"], 4);
    assert_eq!(records.count().await.unwrap(), 1);
    store.close().await.unwrap();
    let reopened = temp.open().await;
    let records = Records::new_at(reopened.clone(), NOW + DAY).await.unwrap();
    assert_eq!(records.retention_days().await.unwrap(), 1);
    assert_eq!(records.stats_at(NOW + DAY).await.unwrap()["total"], 4);
    assert_eq!(records.count().await.unwrap(), 1);
    // Startup cleans physically; the exact cutoff remains for one more millisecond.
    let records = Records::new_at(reopened.clone(), NOW + DAY + 1)
        .await
        .unwrap();
    assert_eq!(records.stats_at(NOW + DAY + 1).await.unwrap()["total"], 4);
    assert_eq!(records.count().await.unwrap(), 0);
    assert_eq!(records.cleanup_at(NOW + DAY + 1).await.unwrap(), 0);
    reopened.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn insert_cleans_expired_and_retention_is_read_from_db_not_stale_handle() {
    let temp = TempDb::new();
    let store = temp.open().await;
    let first = Records::new_at(store.clone(), NOW - 3 * DAY).await.unwrap();
    let second = Records::new_at(store.clone(), NOW - 3 * DAY).await.unwrap();
    first.set_retention_days_at(1, NOW - 3 * DAY).await.unwrap();
    assert_eq!(second.retention_days().await.unwrap(), 1);
    second
        .insert_at(
            "https://example.invalid",
            "",
            &success(),
            false,
            NOW - 3 * DAY,
        )
        .await
        .unwrap();
    second
        .insert_at("https://example.invalid", "", &success(), false, NOW)
        .await
        .unwrap();
    assert_eq!(first.stats_at(NOW).await.unwrap()["total"], 2);
    assert_eq!(first.count().await.unwrap(), 1);
    assert_eq!(first.cleanup_at(NOW).await.unwrap(), 0);
    store.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn search_and_type_filter_before_paging_exact_totals() {
    let temp = TempDb::new();
    let store = temp.open().await;
    let records = Records::new_at(store.clone(), NOW).await.unwrap();
    for i in 0..75 {
        let url = if i < 5 {
            "https://needle.invalid/share"
        } else {
            "https://other.invalid/share"
        };
        let result = json!({"success": i % 2 == 0, "code": if i % 2 == 0 {200} else {403}});
        records
            .insert_at(url, "", &result, i % 3 == 0, NOW + i)
            .await
            .unwrap();
    }
    let list = records
        .list_at(2, 2, "", "NEEDLE.invalid", NOW + 75)
        .await
        .unwrap();
    assert_eq!(list["total"], 5);
    assert_eq!(list["successTotal"], 3);
    assert_eq!(list["failedTotal"], 2);
    assert_eq!(list["pages"], 3);
    assert_eq!(list["success"][0]["id"], 3);
    assert_eq!(list["failed"][0]["id"], 2);
    let failed = records
        .list_at(1, 1, "failed", "needle.invalid", NOW + 75)
        .await
        .unwrap();
    assert_eq!(failed["total"], 2);
    assert_eq!(failed["failed"][0]["id"], 4);
    assert_eq!(failed["successTotal"], 3);
    assert_eq!(
        records.stats_at(NOW + 75).await.unwrap(),
        json!({"total":75,"success":50,"failed":25,"cached":25})
    );
    let empty = records.list_at(999, 50, "all", "", NOW + 75).await.unwrap();
    assert_eq!(empty["total"], 75);
    assert_eq!(empty["success"], json!([]));
    assert_eq!(empty["failed"], json!([]));
    let normalized = records
        .list_at(0, 0, "", "%_' OR 1=1--", NOW + 75)
        .await
        .unwrap();
    assert_eq!(normalized["page"], 1);
    assert_eq!(normalized["limit"], 1);
    assert_eq!(normalized["total"], 0);
    assert_eq!(
        records.list_at(1, 10, "bad", "", NOW).await,
        Err(StorageError::InvalidJson)
    );
    assert_eq!(
        records.list_at(1, 10, "", &"x".repeat(513), NOW).await,
        Err(StorageError::InvalidJson)
    );
    store.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn concurrent_same_millisecond_records_have_unique_ordered_ids() {
    let temp = TempDb::new();
    let store = temp.open().await;
    let records = Records::new_at(store.clone(), NOW).await.unwrap();
    let handles: Vec<_> = (0..64)
        .map(|_| {
            let records = records.clone();
            tokio::spawn(async move {
                records
                    .insert_at("https://example.invalid", "", &success(), false, NOW)
                    .await
                    .unwrap()
            })
        })
        .collect();
    let mut ids = Vec::new();
    for handle in handles {
        ids.push(handle.await.unwrap());
    }
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 64);
    assert_eq!(records.stats_at(NOW).await.unwrap()["total"], 64);
    let list = records.list_at(1, 1000, "", "", NOW).await.unwrap();
    assert_eq!(list["limit"], 50);
    assert_eq!(list["success"][0]["id"], 64);
    assert_eq!(list["success"][49]["id"], 15);
    store.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn private_history_preserves_reparse_data_and_excludes_account_keys() {
    let temp = TempDb::new();
    let store = temp.open().await;
    let records = Records::new_at(store.clone(), NOW).await.unwrap();
    let url = "https://example.invalid/share?pwd=synthetic-share#synthetic-fragment";
    let result = json!({"code":403,"msg":"synthetic upstream error",
        "credentials":"synthetic-account-secret", "data": {
            "files":[{"url":"https://example.invalid/download?sig=synthetic-signature",
                      "pwd":"synthetic-share", "password":"synthetic-share",
                      "access_token":"synthetic-account-secret"}],
            "Cookie":"synthetic-account-secret", "session_key":"synthetic-account-secret",
            "sessionSecret":"synthetic-account-secret", "open_access_token":"synthetic-account-secret",
            "appAccessToken":"synthetic-account-secret", "cookie_login_user":"synthetic-account-secret",
            "SSON":"synthetic-account-secret", "loginInfo":{"token":"synthetic-account-secret"}}});
    records
        .insert_at(url, "synthetic-share", &result, true, NOW)
        .await
        .unwrap();
    let list = records.list_at(1, 10, "", "", NOW).await.unwrap();
    let row = &list["failed"][0];
    assert_eq!(row["url"], url);
    assert_eq!(row["pwd"], "synthetic-share");
    assert_eq!(row["msg"], result["msg"]);
    assert_eq!(row["data"]["files"][0]["password"], "synthetic-share");
    assert_eq!(row["cached"], true);
    assert_eq!(row["truncated"], false);
    assert!(!row.to_string().contains("synthetic-account-secret"));
    for search in [
        "synthetic-share",
        "synthetic-fragment",
        "synthetic-signature",
        "upstream error",
    ] {
        assert_eq!(
            records.list_at(1, 10, "", search, NOW).await.unwrap()["total"],
            1
        );
    }
    assert_eq!(
        records
            .list_at(1, 10, "", "synthetic-account-secret", NOW)
            .await
            .unwrap()["total"],
        0
    );
    store
        .with_connection(|db| {
            let (payload, search): (String, String) = db
                .query_row(
                    "SELECT payload, search_text FROM parse_records",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(|_| StorageError::Database)?;
            assert!(payload.bytes().all(|b| b.is_ascii_hexdigit()));
            assert!(!payload.contains("synthetic-share"));
            assert!(search.contains("synthetic-share")); // Private index, not public telemetry.
            assert!(
                !storage::decrypt_from_kv(&payload)?
                    .to_string()
                    .contains("synthetic-account-secret")
            );
            Ok(())
        })
        .await
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&temp.0).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    store.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn only_selected_page_is_decrypted_and_corruption_returns_static_error() {
    let temp = TempDb::new();
    let store = temp.open().await;
    let records = Records::new_at(store.clone(), NOW).await.unwrap();
    for i in 0..3 {
        records
            .insert_at("https://example.invalid", "", &success(), false, NOW + i)
            .await
            .unwrap();
    }
    store
        .with_connection(|db| {
            db.execute(
                "UPDATE parse_records SET payload='synthetic-invalid-secret' WHERE id=1",
                [],
            )
            .map_err(|_| StorageError::Database)?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(records.stats_at(NOW + 3).await.unwrap()["total"], 3);
    assert_eq!(
        records.list_at(1, 2, "", "", NOW + 3).await.unwrap()["success"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let error = records.list_at(2, 2, "", "", NOW + 3).await.unwrap_err();
    assert_eq!(error, StorageError::InvalidCiphertext);
    assert_eq!(error.to_string(), "storage ciphertext invalid");
    assert_eq!(records.cleanup_at(-1).await, Err(StorageError::Clock));
    store.close().await.unwrap();
    assert_eq!(records.retention_days().await, Err(StorageError::Closed));
}

#[tokio::test(flavor = "current_thread")]
async fn oversized_legacy_text_is_rejected_by_bytes_without_mutating_history() {
    let temp = TempDb::new();
    let store = temp.open().await;
    let records = Records::new_at(store.clone(), NOW).await.unwrap();
    for i in 0..3 {
        records
            .insert_at("https://example.invalid", "", &success(), i == 2, NOW + i)
            .await
            .unwrap();
    }
    let stats = records.stats_at(NOW + 3).await.unwrap();
    let max_ciphertext_bytes = (records::MAX_RECORD_BYTES / 16 + 1) * 32;
    for (name, payload) in [
        ("ascii", "0".repeat(max_ciphertext_bytes + 1)),
        ("unicode", "界".repeat(max_ciphertext_bytes / 3 + 1)),
        ("nul", format!("\0{}", "0".repeat(max_ciphertext_bytes))),
    ] {
        let byte_len = payload.len();
        assert!(byte_len > max_ciphertext_bytes);
        if name == "unicode" {
            assert!(payload.chars().count() < max_ciphertext_bytes);
        }
        store
            .with_connection(move |db| {
                db.execute("UPDATE parse_records SET payload = ?1 WHERE id = 3", [payload])
                    .map_err(|_| StorageError::Database)?;
                let (text_len, blob_len): (usize, usize) = db
                    .query_row(
                        "SELECT length(payload), length(CAST(payload AS BLOB)) FROM parse_records WHERE id = 3",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .map_err(|_| StorageError::Database)?;
                assert_eq!(blob_len, byte_len);
                if name == "unicode" {
                    assert!(text_len < max_ciphertext_bytes);
                } else if name == "nul" {
                    assert_eq!(text_len, 0);
                }
                Ok(())
            })
            .await
            .unwrap();
        // A size failure must happen before malformed hex reaches the decryptor.
        let error = records.list_at(1, 1, "", "", NOW + 3).await.unwrap_err();
        assert_eq!(error, StorageError::InvalidJson, "{name}");
        assert_eq!(error.to_string(), "storage JSON invalid");
        assert_eq!(records.stats_at(NOW + 3).await.unwrap(), stats, "{name}");
        assert_eq!(records.count().await.unwrap(), 3, "{name}");
        let next = records.list_at(2, 1, "", "", NOW + 3).await.unwrap();
        assert_eq!(next["total"], 3);
        assert_eq!(next["successTotal"], 3);
        assert_eq!(next["success"][0]["id"], 2);
        store
            .with_connection(move |db| {
                let remaining_bytes: usize = db
                    .query_row(
                        "SELECT length(CAST(payload AS BLOB)) FROM parse_records WHERE id = 3",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(|_| StorageError::Database)?;
                assert_eq!(remaining_bytes, byte_len);
                Ok(())
            })
            .await
            .unwrap();
    }
    store.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn legacy_blob_payload_boundaries_return_errors_without_poisoning_storage() {
    let temp = TempDb::new();
    let store = temp.open().await;
    let records = Records::new_at(store.clone(), NOW).await.unwrap();
    for i in 0..2 {
        records
            .insert_at("https://example.invalid", "", &success(), i == 1, NOW + i)
            .await
            .unwrap();
    }
    let stats = records.stats_at(NOW + 2).await.unwrap();
    let max_ciphertext_bytes = (records::MAX_RECORD_BYTES / 16 + 1) * 32;
    for byte_len in [
        0,
        max_ciphertext_bytes - 1,
        max_ciphertext_bytes,
        max_ciphertext_bytes + 1,
    ] {
        store
            .with_connection(move |db| {
                // BLOB is not TEXT, even if its size is acceptable; invalid UTF-8
                // must not panic the SQLite worker or be coerced into a String.
                db.execute(
                    "UPDATE parse_records SET payload = ?1 WHERE id = 2",
                    [vec![0xff_u8; byte_len]],
                )
                .map_err(|_| StorageError::Database)?;
                Ok(())
            })
            .await
            .unwrap();
        let expected = if byte_len > max_ciphertext_bytes {
            StorageError::InvalidJson
        } else {
            StorageError::Database
        };
        assert_eq!(
            records.list_at(1, 1, "", "", NOW + 2).await,
            Err(expected),
            "{byte_len}"
        );
        assert_eq!(records.stats_at(NOW + 2).await.unwrap(), stats);
        assert_eq!(records.count().await.unwrap(), 2);
        let next = records.list_at(2, 1, "", "", NOW + 2).await.unwrap();
        assert_eq!(next["total"], 2);
        assert_eq!(next["success"][0]["id"], 1);
        store
            .with_connection(move |db| {
                let (kind, remaining_bytes): (String, usize) = db
                    .query_row(
                        "SELECT typeof(payload), length(payload) FROM parse_records WHERE id = 2",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .map_err(|_| StorageError::Database)?;
                assert_eq!(kind, "blob");
                assert_eq!(remaining_bytes, byte_len);
                Ok(())
            })
            .await
            .unwrap();
    }
    store.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn valid_legacy_ciphertext_at_padded_byte_limit_is_still_readable() {
    let temp = TempDb::new();
    let store = temp.open().await;
    let records = Records::new_at(store.clone(), NOW).await.unwrap();
    records
        .insert_at("https://example.invalid", "", &success(), false, NOW)
        .await
        .unwrap();
    let stats = records.stats_at(NOW).await.unwrap();
    let max_ciphertext_bytes = (records::MAX_RECORD_BYTES / 16 + 1) * 32;
    for plaintext_bytes in [records::MAX_RECORD_BYTES - 1, records::MAX_RECORD_BYTES] {
        let mut payload = json!({"url":"https://example.invalid", "pwd":"", "code":200,
            "msg":"synthetic-success", "data":"", "truncated":false});
        let data_len = plaintext_bytes - payload.to_string().len();
        payload["data"] = json!("x".repeat(data_len));
        assert_eq!(payload.to_string().len(), plaintext_bytes);
        let encrypted = storage::encrypt_for_kv(&payload).unwrap();
        assert_eq!(encrypted.len(), (plaintext_bytes / 16 + 1) * 32);
        if plaintext_bytes == records::MAX_RECORD_BYTES {
            // An aligned plaintext needs a whole extra AES padding block.
            assert_eq!(encrypted.len(), max_ciphertext_bytes);
        }
        store
            .with_connection(move |db| {
                db.execute(
                    "UPDATE parse_records SET payload = ?1 WHERE id = 1",
                    [encrypted],
                )
                .map_err(|_| StorageError::Database)?;
                Ok(())
            })
            .await
            .unwrap();
        let page = records.list_at(1, 1, "", "", NOW).await.unwrap();
        assert_eq!(page["total"], 1);
        assert_eq!(page["success"][0]["id"], 1);
        assert_eq!(page["success"][0]["truncated"], false);
        assert_eq!(page["success"][0]["data"], payload["data"]);
        assert_eq!(records.stats_at(NOW).await.unwrap(), stats);
        assert_eq!(records.count().await.unwrap(), 1);
    }
    store.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn settings_and_cleanup_rollback_together_on_delete_failure() {
    let temp = TempDb::new();
    let store = temp.open().await;
    let records = Records::new_at(store.clone(), NOW - 2 * DAY).await.unwrap();
    records
        .insert_at(
            "https://example.invalid",
            "",
            &success(),
            false,
            NOW - 2 * DAY,
        )
        .await
        .unwrap();
    store.with_connection(|db| {
        db.execute_batch("CREATE TRIGGER synthetic_failure BEFORE DELETE ON parse_records BEGIN SELECT RAISE(ABORT, 'synthetic-secret'); END;")
            .map_err(|_| StorageError::Database)
    }).await.unwrap();
    let error = records.set_retention_days_at(1, NOW).await.unwrap_err();
    assert_eq!(error, StorageError::Database);
    assert_eq!(error.to_string(), "storage database operation failed");
    assert_eq!(records.retention_days().await.unwrap(), 7);
    assert_eq!(records.stats_at(NOW).await.unwrap()["total"], 1);
    assert_eq!(
        records
            .insert_at(
                "https://example.invalid",
                "",
                &success(),
                false,
                NOW + 8 * DAY
            )
            .await,
        Err(StorageError::Database)
    );
    assert_eq!(records.stats_at(NOW).await.unwrap()["total"], 1);
    store.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn frozen_default_classification_and_full_frontend_projection() {
    let fixture = support::oracle("records", include_str!("fixtures/records.json"));
    assert_eq!(
        fixture["default_stats"],
        json!({"total":0,"success":0,"failed":0,"cached":0})
    );
    assert_eq!(fixture["legacy_retention_seconds"], 7 * 24 * 3600);
    let temp = TempDb::new();
    let store = temp.open().await;
    let records = Records::new_at(store.clone(), NOW).await.unwrap();
    assert_eq!(records.retention_days().await.unwrap(), 7);
    assert_eq!(
        records.stats_at(NOW).await.unwrap(),
        fixture["default_stats"]
    );
    for vector in fixture["classification"].as_array().unwrap() {
        records
            .insert_at(
                vector["saved_record"]["url"].as_str().unwrap(),
                vector["saved_record"]["pwd"].as_str().unwrap(),
                &vector["result"],
                false,
                NOW,
            )
            .await
            .unwrap();
        let latest = records.list_at(1, 1, "", "", NOW).await.unwrap();
        let target = if vector["new_success"] == true {
            "success"
        } else {
            "failed"
        };
        assert_eq!(vector["new_success"], vector["legacy_success"]);
        for key in ["url", "pwd", "success", "code", "msg", "data", "timestamp"] {
            assert_eq!(
                latest[target][0][key], vector["frontend_record"][key],
                "{} field {key}",
                vector["name"]
            );
        }
        assert_eq!(
            latest[target].as_array().unwrap().len(),
            1,
            "{}",
            vector["name"]
        );
    }
    let list = records.list_at(1, 10, "", "", NOW).await.unwrap();
    for key in fixture["list_keys"].as_array().unwrap() {
        assert!(list.get(key.as_str().unwrap()).is_some());
    }
    // Public real-clock entrypoints are usable without a new runtime or connection.
    let real = Records::new(store.clone()).await.unwrap();
    real.set_retention_days(7).await.unwrap();
    real.insert("https://example.invalid", "", &success(), true)
        .await
        .unwrap();
    assert_eq!(
        real.stats().await.unwrap()["total"],
        fixture["classification"].as_array().unwrap().len() + 1
    );
    assert_eq!(
        real.list(1, 10, "", "").await.unwrap()["success"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    store.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn deletion_apis_only_touch_history_and_keep_lifetime_stats() {
    let temp = TempDb::new();
    let store = temp.open().await;
    store
        .put("credential_synthetic", json!({"synthetic":true}), None)
        .await
        .unwrap();
    store
        .put("parse_record_legacy", json!({"synthetic":true}), None)
        .await
        .unwrap();
    let records = Records::new_at(store.clone(), NOW).await.unwrap();
    let mut ids = Vec::new();
    for i in 0..6 {
        ids.push(
            records
                .insert_at(
                    "https://example.invalid/share",
                    "synthetic-share",
                    &json!({"success": i % 2 == 0, "code":if i % 2 == 0 {200} else {403}}),
                    i == 0 || i == 2,
                    NOW + i,
                )
                .await
                .unwrap(),
        );
    }
    let stats = json!({"total":6,"success":3,"failed":3,"cached":2});
    assert_eq!(records.count().await.unwrap(), 6);
    assert!(records.delete(ids[3]).await.unwrap());
    assert!(!records.delete(ids[3]).await.unwrap());
    assert!(!records.delete(-1).await.unwrap());
    assert_eq!(records.delete_batch(2).await.unwrap(), 2);
    let list = records.list_at(1, 10, "", "", NOW + 6).await.unwrap();
    assert_eq!(list["total"], 3);
    assert_eq!(list["success"][1]["id"], ids[2]);
    for limit in [0, 1001, u32::MAX] {
        assert_eq!(
            records.delete_batch(limit).await,
            Err(StorageError::InvalidJson)
        );
    }
    assert_eq!(records.clear().await.unwrap(), 3);
    assert_eq!(records.clear().await.unwrap(), 0);
    assert_eq!(records.delete_batch(1000).await.unwrap(), 0);
    assert_eq!(records.count().await.unwrap(), 0);
    assert_eq!(records.stats_at(NOW + 99 * DAY).await.unwrap(), stats);
    for key in ["credential_synthetic", "parse_record_legacy"] {
        assert_eq!(
            store.get(key).await.unwrap(),
            Some(json!({"synthetic":true}))
        );
    }
    store.close().await.unwrap();
    let reopened = temp.open().await;
    let records = Records::new_at(reopened.clone(), NOW + 99 * DAY)
        .await
        .unwrap();
    assert_eq!(records.stats_at(NOW + 99 * DAY).await.unwrap(), stats);
    assert!(
        records
            .insert_at(
                "https://example.invalid",
                "",
                &success(),
                false,
                NOW + 99 * DAY
            )
            .await
            .unwrap()
            > ids[5]
    );
    reopened.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn large_data_is_explicitly_truncated_and_page_overflow_is_error() {
    let temp = TempDb::new();
    let store = temp.open().await;
    let records = Records::new_at(store.clone(), NOW).await.unwrap();
    let url = "https://example.invalid/share?pwd=synthetic-share";
    let huge = json!({"code":403,"msg":"synthetic-error","data": "x".repeat(2 * records::MAX_RECORD_BYTES)});
    records
        .insert_at(url, "synthetic-share", &huge, true, NOW)
        .await
        .unwrap();
    let row = records.list_at(1, 1, "", "", NOW).await.unwrap()["failed"][0].clone();
    assert_eq!(row["url"], url);
    assert_eq!(row["pwd"], "synthetic-share");
    assert_eq!(row["msg"], "synthetic-error");
    assert_eq!(row["data"], Value::Null);
    assert_eq!(row["truncated"], true);
    let large = json!({"code":200,"data": "x".repeat(900_000)});
    for i in 1..=5 {
        records
            .insert_at(url, "synthetic-share", &large, false, NOW + i)
            .await
            .unwrap();
    }
    assert_eq!(
        records.list_at(1, 5, "", "", NOW + 6).await,
        Err(StorageError::InvalidJson)
    );
    let small = records.list_at(1, 4, "", "", NOW + 6).await.unwrap();
    assert!(small.to_string().len() <= records::MAX_PAGE_BYTES);
    assert_eq!(small["total"], 6);
    assert_eq!(small["success"][0]["truncated"], false);
    assert_eq!(small["success"][0]["data"].as_str().unwrap().len(), 900_000);
    let prior = records.stats_at(NOW + 6).await.unwrap();
    assert_eq!(
        records
            .insert_at(
                &"x".repeat(records::MAX_RECORD_BYTES + 1),
                "",
                &success(),
                false,
                NOW + 6
            )
            .await,
        Err(StorageError::InvalidJson)
    );
    assert_eq!(records.stats_at(NOW + 6).await.unwrap(), prior);
    store.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn stats_update_failure_rolls_back_insert_and_cleanup() {
    let temp = TempDb::new();
    let store = temp.open().await;
    let records = Records::new_at(store.clone(), NOW - 8 * DAY).await.unwrap();
    records
        .insert_at(
            "https://example.invalid",
            "",
            &success(),
            false,
            NOW - 8 * DAY,
        )
        .await
        .unwrap();
    store.with_connection(|db| {
        db.execute_batch("CREATE TRIGGER synthetic_stats_failure BEFORE UPDATE ON records_stats BEGIN SELECT RAISE(ABORT, 'synthetic-secret'); END;")
            .map_err(|_| StorageError::Database)
    }).await.unwrap();
    assert_eq!(
        records
            .insert_at("https://example.invalid", "", &success(), true, NOW)
            .await,
        Err(StorageError::Database)
    );
    assert_eq!(records.count().await.unwrap(), 1);
    assert_eq!(
        records.stats_at(NOW).await.unwrap(),
        json!({"total":1,"success":1,"failed":0,"cached":0})
    );
    store.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn prior_component_upgrade_seeds_surviving_rows_once_before_cleanup() {
    let temp = TempDb::new();
    let store = temp.open().await;
    let records = Records::new_at(store.clone(), NOW - 8 * DAY).await.unwrap();
    records
        .insert_at(
            "https://example.invalid",
            "",
            &success(),
            true,
            NOW - 8 * DAY,
        )
        .await
        .unwrap();
    store
        .with_connection(|db| {
            db.execute_batch("DROP TABLE records_stats;")
                .map_err(|_| StorageError::Database)
        })
        .await
        .unwrap();
    let records = Records::new_at(store.clone(), NOW).await.unwrap();
    assert_eq!(records.count().await.unwrap(), 0);
    assert_eq!(
        records.stats_at(NOW).await.unwrap(),
        json!({"total":1,"success":1,"failed":0,"cached":1})
    );
    let records = Records::new_at(store.clone(), NOW).await.unwrap();
    assert_eq!(records.stats_at(NOW).await.unwrap()["total"], 1);
    store.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn joined_outcomes_never_miscount_failure_as_cached_success() {
    let store = Storage::open(":memory:", 16).await.unwrap();
    let records = Records::new_at(store.clone(), NOW).await.unwrap();
    records
        .insert_at(
            "https://example.invalid",
            "",
            &json!({"code":501,"success":false}),
            false,
            NOW,
        )
        .await
        .unwrap();
    records.record_request_outcome(false, false).await.unwrap();
    assert_eq!(
        records.stats_at(NOW).await.unwrap(),
        json!({"total":2,"success":0,"failed":2,"cached":0})
    );
    assert_eq!(records.count().await.unwrap(), 1);
    assert_eq!(
        records.record_request_outcome(false, true).await,
        Err(StorageError::InvalidJson)
    );
    records.record_request_outcome(true, false).await.unwrap();
    records.record_cache_hit().await.unwrap();
    assert_eq!(
        records.stats_at(NOW).await.unwrap(),
        json!({"total":4,"success":2,"failed":2,"cached":1})
    );
    assert_eq!(records.count().await.unwrap(), 1);
    store.close().await.unwrap();
}
