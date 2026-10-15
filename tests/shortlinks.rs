mod support;
use jxpan_rust::{records, shortlinks, storage};

use rusqlite::params;
use serde_json::Value;
use shortlinks::{MAX_SHORT_LINKS, MAX_URL_BYTES, ShortLinkError, ShortLinks, validate_parser_url};
use std::path::PathBuf;
use storage::{Storage, StorageError};

const ORIGINAL: &str = "https://parser.example.invalid/?url=https%3A%2F%2Fcloud.189.cn%2Ft%2FSynthetic&pwd=synthetic%2Bpwd&type=down&fid=root&id=file";
struct TempDb(PathBuf);
impl TempDb {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("jxpan-shortlinks-{}.sqlite", uuid::Uuid::new_v4())))
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
fn corpus() -> Value {
    support::oracle("shortlinks", include_str!("fixtures/shortlinks.json"))
}
async fn counts(store: &Storage, code: &str) -> (i64, i64, i64) {
    let code = code.to_owned();
    store
        .with_connection(move |db| {
            db.query_row(
        "SELECT access_count, last_accessed, expires_at FROM short_links WHERE shortcode = ?1",
        [code], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .map_err(|_| StorageError::Database)
        })
        .await
        .unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn frozen_supported_parser_matrix_and_passwords() {
    let corpus = corpus();
    assert_eq!(
        corpus["source_sha256"],
        "47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118"
    );
    let vectors = corpus["vectors"].as_array().unwrap();
    assert_eq!(vectors.len(), 96);
    let storage = Storage::open(":memory:", 256).await.unwrap();
    let links = ShortLinks::new(storage.clone()).await.unwrap();
    for vector in vectors {
        let original = vector["original_url"].as_str().unwrap();
        let code = links.create(original).await.unwrap();
        assert_eq!(code.len(), 6);
        assert!(code.bytes().all(|b| b.is_ascii_alphanumeric()));
        // Canonical duplicates now share a code; the frozen per-resolve increment
        // still applies, rather than resetting that link's accumulated counter.
        let before = counts(&storage, &code).await.0;
        let target = links.resolve(&code).await.unwrap().unwrap();
        assert_eq!(target, vector["expected"].as_str().unwrap(), "{vector}");
        let parsed = url::Url::parse(&format!("https://safe-origin.invalid{target}")).unwrap();
        assert_eq!(
            parsed.query_pairs().find(|p| p.0 == "pwd").unwrap().1,
            "合成 +&密码"
        );
        assert_eq!(
            counts(&storage, &code).await.0,
            before + vector["count"].as_i64().unwrap()
        );
    }
    // URLSearchParams.get first duplicate, id || d, empty omission, folder compaction.
    let mut original = url::Url::parse(ORIGINAL).unwrap();
    original.set_query(Some("url=https%3A%2F%2Fcloud.189.cn%2Ft%2FSynthetic&id=&id=ignored&d=alias&fid=&fid2=root&fid3=root&fid20=leaf&type=json"));
    assert_eq!(
        validate_parser_url(original.as_str()).unwrap(),
        "/?url=https%3A%2F%2Fcloud.189.cn%2Ft%2FSynthetic&type=json&id=alias&fid=root&fid2=leaf"
    );
    // Ignored duplicate targets are never executed OR persisted as secret-bearing raw input.
    let duplicate = format!("{ORIGINAL}&url=https%3A%2F%2Fevil.invalid%2F%3Ftoken%3Dsynthetic");
    let code = links.create(&duplicate).await.unwrap();
    assert_eq!(
        links.resolve(&code).await.unwrap(),
        Some(validate_parser_url(ORIGINAL).unwrap())
    );
    let saved: String = storage
        .with_connection(move |db| {
            db.query_row(
                "SELECT original_url FROM short_links WHERE shortcode = ?1",
                [code],
                |r| r.get(0),
            )
            .map_err(|_| StorageError::Database)
        })
        .await
        .unwrap();
    assert!(!saved.contains("evil.invalid"));
    assert!(!saved.contains("token"));
    let too_many = format!("{ORIGINAL}{}", "&pwd=synthetic".repeat(65));
    assert_eq!(
        links.create(&too_many).await,
        Err(ShortLinkError::InvalidUrl)
    );
    storage.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn capacity_reuses_existing_links_and_never_evicts_history() {
    let storage = Storage::open(":memory:", 16).await.unwrap();
    let links = ShortLinks::new(storage.clone()).await.unwrap();
    assert_eq!(MAX_SHORT_LINKS, 10_000);
    storage.with_connection(|db| {
        let tx = db.transaction().map_err(|_| StorageError::Database)?;
        {
            let mut insert = tx.prepare(
                "INSERT INTO short_links
                 (shortcode, original_url, created_at, access_count, last_accessed, expires_at)
                 VALUES (?1, ?2, 1, 7, 2, 1)",
            ).map_err(|_| StorageError::Database)?;
            for i in 0..MAX_SHORT_LINKS - 1 {
                let target = format!("https://parser.example.invalid/?url=https%3A%2F%2Fcloud.189.cn%2Ft%2FSynthetic&id=seed{i}");
                insert.execute(params![format!("{i:06}"), target]).map_err(|_| StorageError::Database)?;
            }
        }
        tx.commit().map_err(|_| StorageError::Database)
    })
    .await
    .unwrap();

    let code = links.create(ORIGINAL).await.unwrap();
    assert_eq!(links.create(ORIGINAL).await.unwrap(), code);
    let canonical = format!(
        "https://parser.example.invalid{}",
        validate_parser_url(ORIGINAL).unwrap()
    );
    assert_eq!(links.create(&canonical).await.unwrap(), code);
    assert_eq!(
        links.create(&format!("{ORIGINAL}&sid=new-target")).await,
        Err(ShortLinkError::Capacity)
    );
    assert_eq!(
        links.resolve(&code).await.unwrap(),
        Some(validate_parser_url(ORIGINAL).unwrap())
    );
    assert_eq!(counts(&storage, "000000").await, (7, 2, 1));
    assert!(links.resolve("000000").await.unwrap().is_some());
    assert_eq!(counts(&storage, "000000").await.0, 8);
    let totals = storage
        .with_connection(|db| {
            db.query_row(
                "SELECT count(*), sum(created_at = 1), sum(expires_at = 1) FROM short_links",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .map_err(|_| StorageError::Database)
        })
        .await
        .unwrap();
    assert_eq!(
        totals,
        (
            MAX_SHORT_LINKS as i64,
            MAX_SHORT_LINKS as i64 - 1,
            MAX_SHORT_LINKS as i64 - 1
        )
    );

    // A legacy database already above the new admission limit is not pruned.
    storage.with_connection(|db| {
        db.execute(
            "INSERT INTO short_links (shortcode, original_url, created_at) VALUES ('Beyond', ?1, 1)",
            ["https://parser.example.invalid/?url=https%3A%2F%2Fcloud.189.cn%2Ft%2FSynthetic&id=beyond"],
        ).map(|_| ()).map_err(|_| StorageError::Database)
    }).await.unwrap();
    let reopened = ShortLinks::new(storage.clone()).await.unwrap();
    assert_eq!(reopened.create(ORIGINAL).await.unwrap(), code);
    assert_eq!(
        reopened.create("https://parser.example.invalid/?url=https%3A%2F%2Fcloud.189.cn%2Ft%2FSynthetic&id=beyond").await.unwrap(),
        "Beyond"
    );
    assert_eq!(
        reopened
            .create(&format!("{ORIGINAL}&sid=another-new-target"))
            .await,
        Err(ShortLinkError::Capacity)
    );
    let count = storage
        .with_connection(|db| {
            db.query_row("SELECT count(*) FROM short_links", [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(|_| StorageError::Database)
        })
        .await
        .unwrap();
    assert_eq!(count, MAX_SHORT_LINKS as i64 + 1);
    assert!(reopened.resolve("000000").await.unwrap().is_some());
    storage.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn duplicate_creation_is_atomic_across_storage_instances() {
    let temp = TempDb::new();
    let storage = temp.open().await;
    let other_store = temp.open().await;
    let links = ShortLinks::new(storage.clone()).await.unwrap();
    let other = ShortLinks::new(other_store.clone()).await.unwrap();
    let (a, b) = tokio::join!(links.create(ORIGINAL), other.create(ORIGINAL));
    let code = a.unwrap();
    assert_eq!(b.unwrap(), code);
    assert_eq!(links.create(ORIGINAL).await.unwrap(), code);
    let count = storage
        .with_connection(|db| {
            db.query_row("SELECT count(*) FROM short_links", [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(|_| StorageError::Database)
        })
        .await
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(counts(&storage, &code).await.0, 0);
    storage.close().await.unwrap();
    other_store.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn persistence_atomic_counters_and_retention_independence() {
    let temp = TempDb::new();
    let storage = temp.open().await;
    let links = ShortLinks::new(storage.clone()).await.unwrap();
    let code = links.create(ORIGINAL).await.unwrap();
    let created: i64 = storage
        .with_connection({
            let code = code.clone();
            move |db| {
                db.query_row(
                    "SELECT created_at FROM short_links WHERE shortcode = ?1",
                    [code],
                    |r| r.get(0),
                )
                .map_err(|_| StorageError::Database)
            }
        })
        .await
        .unwrap();
    assert!(created > 0);
    storage.close().await.unwrap();
    let storage = temp.open().await;
    let links = ShortLinks::new(storage.clone()).await.unwrap();
    let other_store = temp.open().await;
    let other = ShortLinks::new(other_store.clone()).await.unwrap();
    let mut tasks = Vec::new();
    for i in 0..64 {
        let links = if i % 2 == 0 {
            links.clone()
        } else {
            other.clone()
        };
        let code = code.clone();
        tasks.push(tokio::spawn(async move { links.resolve(&code).await }));
    }
    for task in tasks {
        assert_eq!(
            task.await.unwrap().unwrap(),
            Some(validate_parser_url(ORIGINAL).unwrap())
        );
    }
    let count = counts(&storage, &code).await;
    assert_eq!(count.0, 64);
    assert!(count.1 >= created);
    assert_eq!(count.2, 0);
    // Log retention is a distinct table/repository; cleanup cannot touch short links.
    let records = records::Records::new_at(storage.clone(), 1_700_000_000_000)
        .await
        .unwrap();
    records
        .set_retention_days_at(1, 1_700_000_000_000)
        .await
        .unwrap();
    records.cleanup_at(1_900_000_000_000).await.unwrap();
    assert_eq!(counts(&storage, &code).await.0, 64);
    assert!(links.resolve(&code).await.unwrap().is_some());
    storage.close().await.unwrap();
    other_store.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn frozen_expiry_is_not_enforced_and_access_counts_increment() {
    let storage = Storage::open(":memory:", 16).await.unwrap();
    let links = ShortLinks::new(storage.clone()).await.unwrap();
    for vector in corpus()["expiry"].as_array().unwrap() {
        let expiry = vector["expires_at"].as_i64();
        storage
            .with_connection(move |db| {
                db.execute(
                    "INSERT OR REPLACE INTO short_links
            (shortcode, original_url, access_count, expires_at) VALUES ('AAAAAA', ?1, 3, ?2)",
                    params![ORIGINAL, expiry],
                )
                .map(|_| ())
                .map_err(|_| StorageError::Database)
            })
            .await
            .unwrap();
        let result = links.resolve("AAAAAA").await.unwrap();
        assert_eq!(result.is_some(), vector["present"].as_bool().unwrap());
        let actual: i64 = storage
            .with_connection(|db| {
                db.query_row(
                    "SELECT access_count FROM short_links WHERE shortcode = 'AAAAAA'",
                    [],
                    |r| r.get(0),
                )
                .map_err(|_| StorageError::Database)
            })
            .await
            .unwrap();
        assert_eq!(actual, vector["stored_count"].as_i64().unwrap());
    }
    assert_eq!(links.resolve("ZZZZZZ").await.unwrap(), None);
    storage.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn invalid_urls_never_persist_and_legacy_rows_cannot_execute_actions() {
    let storage = Storage::open(":memory:", 16).await.unwrap();
    let links = ShortLinks::new(storage.clone()).await.unwrap();
    let mut invalid = vec![
        "".to_owned(),
        "/?url=https://cloud.189.cn/t/Synthetic".into(),
        "javascript:alert(1)".into(),
        "https://parser.example.invalid/admin?url=https://cloud.189.cn/t/Synthetic".into(),
        "https://user:pass@parser.example.invalid/?url=https://cloud.189.cn/t/Synthetic".into(),
        "https://@parser.example.invalid/?url=https://cloud.189.cn/t/Synthetic".into(),
        "https://parser.example.invalid/?url=https://cloud.189.cn/t/Synthetic#fragment".into(),
        format!("{ORIGINAL}&auth=synthetic"),
        format!("{ORIGINAL}&c189_token=synthetic"),
        format!("{ORIGINAL}&action=logout"),
        format!("{ORIGINAL}&share_id=synthetic"),
        format!("{ORIGINAL}&cookie=synthetic"),
        format!("{ORIGINAL}&fid21=unsupported"),
        format!("{ORIGINAL}&json=1"),
        format!("{ORIGINAL}&token=synthetic"),
        "x".repeat(MAX_URL_BYTES + 1),
        format!("{ORIGINAL}&pwd={}", "x".repeat(4097)),
    ];
    for nested in [
        "ftp://cloud.189.cn/t/Synthetic",
        "cloud.189.cn/t/Synthetic",
        "https://evil.invalid/t/Synthetic",
        "https://cloud.189.cn.evil.invalid/t/Synthetic",
        "https://user:pass@cloud.189.cn/t/Synthetic",
        "https://cloud.189.cn/api/open/login?code=Synthetic",
        "https://cloud.189.cn/t/Synthetic?token=synthetic",
        "https://yun.139.com/shareweb/#/w/i/Synthetic?auth=synthetic",
        "https://cloud.189.cn:444/t/Synthetic",
    ] {
        let mut url = url::Url::parse("https://parser.example.invalid/").unwrap();
        url.query_pairs_mut().append_pair("url", nested);
        invalid.push(url.into());
    }
    for original in invalid {
        assert_eq!(
            links.create(&original).await,
            Err(ShortLinkError::InvalidUrl)
        );
        let stored = original.clone();
        storage
            .with_connection(move |db| {
                db.execute(
                    "INSERT OR REPLACE INTO short_links (shortcode, original_url)
            VALUES ('AAAAAA', ?1)",
                    [stored],
                )
                .map(|_| ())
                .map_err(|_| StorageError::Database)
            })
            .await
            .unwrap();
        assert_eq!(
            links.resolve("AAAAAA").await,
            Err(ShortLinkError::InvalidUrl)
        );
        assert_eq!(counts(&storage, "AAAAAA").await.0, 0);
    }
    for code in [
        "",
        "a",
        "abcdefg",
        "abc/12",
        "abc?12",
        "abc\n12",
        "éabcd",
        "' OR 1=1 --",
    ] {
        assert_eq!(links.resolve(code).await, Err(ShortLinkError::InvalidCode));
    }
    storage.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn safe_contract_preserves_sid_and_never_returns_an_origin() {
    let storage = Storage::open(":memory:", 16).await.unwrap();
    let links = ShortLinks::new(storage.clone()).await.unwrap();
    let original = format!("{ORIGINAL}&sid=SyntheticSID");
    let code = links.create(&original).await.unwrap();
    let local = links.resolve(&code).await.unwrap().unwrap();
    assert!(local.starts_with("/?"));
    assert!(!local.starts_with("//"));
    assert!(local.ends_with("&sid=SyntheticSID&fid=root"));
    // Parent can only join against its trusted service origin, not a stored origin.
    let safe = url::Url::parse("https://trusted.example.invalid/")
        .unwrap()
        .join(&local)
        .unwrap();
    assert_eq!(safe.host_str(), Some("trusted.example.invalid"));
    let facts = corpus();
    assert!(facts["route_facts"][1]["main"].as_bool().unwrap()); // /shorten is NOT deployed
    assert_eq!(
        facts["route_facts"][1]["url"],
        "https://parser.example.invalid/shorten"
    );
    assert!(
        !facts["route_facts"][4]["url"]
            .as_str()
            .unwrap()
            .contains("sid=")
    ); // deliberate fix
    assert!(
        facts["route_facts"][5]["url"]
            .as_str()
            .unwrap()
            .contains("action=synthetic_unsafe")
    ); // deliberately NOT inherited
    storage.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn database_errors_are_redacted_not_not_found_and_updates_roll_back() {
    let storage = Storage::open(":memory:", 16).await.unwrap();
    let links = ShortLinks::new(storage.clone()).await.unwrap();
    let code = links.create(ORIGINAL).await.unwrap();
    storage
        .with_connection(|db| {
            db.execute_batch(
                "CREATE TRIGGER fail_short_access BEFORE UPDATE ON short_links
        BEGIN SELECT RAISE(ABORT, 'synthetic-sensitive-error'); END;",
            )
            .map_err(|_| StorageError::Database)
        })
        .await
        .unwrap();
    assert_eq!(
        links.resolve(&code).await,
        Err(ShortLinkError::Storage(StorageError::Database))
    );
    assert_eq!(counts(&storage, &code).await.0, 0);
    assert!(
        !ShortLinkError::Storage(StorageError::Database)
            .to_string()
            .contains("synthetic")
    );
    storage
        .with_connection(|db| {
            db.execute_batch("DROP TABLE short_links;")
                .map_err(|_| StorageError::Database)
        })
        .await
        .unwrap();
    assert_eq!(
        links.resolve(&code).await,
        Err(ShortLinkError::Storage(StorageError::Database))
    );
    storage.close().await.unwrap();
    assert_eq!(
        links.create(ORIGINAL).await,
        Err(ShortLinkError::Storage(StorageError::Closed))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn queue_is_bounded_and_executor_remains_responsive() {
    use futures_util::poll;
    let storage = Storage::open(":memory:", 1).await.unwrap();
    let links = ShortLinks::new(storage.clone()).await.unwrap();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let active = tokio::spawn({
        let storage = storage.clone();
        async move {
            storage
                .with_connection(move |_| {
                    let _ = started_tx.send(());
                    let _ = release_rx.recv();
                    Ok(())
                })
                .await
        }
    });
    started_rx.await.unwrap();
    let mut queued = Box::pin(links.create(ORIGINAL));
    assert!(poll!(queued.as_mut()).is_pending());
    assert_eq!(
        links.create(ORIGINAL).await,
        Err(ShortLinkError::Storage(StorageError::QueueFull))
    );
    tokio::task::yield_now().await;
    release_tx.send(()).unwrap();
    active.await.unwrap().unwrap();
    queued.await.unwrap();
    storage.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn incompatible_schema_is_rejected_without_rewriting_old_rows() {
    let storage = Storage::open(":memory:", 16).await.unwrap();
    storage
        .with_connection(|db| {
            db.execute_batch(
                "CREATE TABLE short_links (shortcode TEXT, original_url TEXT NOT NULL,
        created_at INTEGER, access_count INTEGER, last_accessed INTEGER, expires_at INTEGER);
        INSERT INTO short_links VALUES ('AAAAAA', 'synthetic', 1, 2, 3, 4);",
            )
            .map_err(|_| StorageError::Database)
        })
        .await
        .unwrap();
    assert!(matches!(
        ShortLinks::new(storage.clone()).await,
        Err(ShortLinkError::Storage(StorageError::Schema))
    ));
    let count = storage
        .with_connection(|db| {
            db.query_row("SELECT COUNT(*) FROM short_links", [], |r| {
                r.get::<_, i64>(0)
            })
            .map_err(|_| StorageError::Database)
        })
        .await
        .unwrap();
    assert_eq!(count, 1);
    storage.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn all_twenty_folder_aliases_preserve_order_through_shortlink_roundtrip() {
    let storage = Storage::open(":memory:", 16).await.unwrap();
    let links = ShortLinks::new(storage.clone()).await.unwrap();
    for count in 0..=20 {
        let mut original = url::Url::parse(ORIGINAL).unwrap();
        for i in 1..=count {
            original
                .query_pairs_mut()
                .append_pair(&format!("fid{i}"), &format!("folder{i}"));
        }
        let code = links.create(original.as_str()).await.unwrap();
        let target = links.resolve(&code).await.unwrap().unwrap();
        let params = jxpan_rust::http::params::Params::query(target.split_once('?').map(|p| p.1));
        let expected: Vec<_> = std::iter::once("root".to_owned())
            .chain((1..=count).map(|i| format!("folder{i}")))
            .collect();
        assert_eq!(params.folders(), expected, "alias count {count}");
        let canonical = format!("https://safe-origin.invalid{target}");
        assert_eq!(validate_parser_url(&canonical).unwrap(), target);
    }
    storage.close().await.unwrap();
}
