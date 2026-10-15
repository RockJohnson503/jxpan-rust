//! Persistent parser tuning and a process-wide, cancellation-safe upstream budget.
//! Settings affect scheduling only; caches, credentials and history are not rewritten.
use crate::storage::{Result, Storage, StorageError, encrypt_for_kv};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::sync::{Mutex as AsyncMutex, Notify};

pub const DEFAULT_CONCURRENCY: usize = 16;
pub const MAX_CONCURRENCY: usize = 16;
const KEY: &str = "rust_parse_settings";

struct Budget {
    limit: usize,
    active: usize,
}
struct Inner {
    budget: Mutex<Budget>,
    changed: Notify,
}
#[derive(Clone)]
pub struct UpstreamLimit(Arc<Inner>);
impl Default for UpstreamLimit {
    fn default() -> Self {
        Self::new(DEFAULT_CONCURRENCY)
    }
}
impl UpstreamLimit {
    pub fn new(limit: usize) -> Self {
        assert!((1..=MAX_CONCURRENCY).contains(&limit));
        Self(Arc::new(Inner {
            budget: Mutex::new(Budget { limit, active: 0 }),
            changed: Notify::new(),
        }))
    }
    pub fn concurrency(&self) -> usize {
        self.0
            .budget
            .lock()
            .expect("upstream budget poisoned")
            .limit
    }
    fn set(&self, limit: usize) {
        self.0
            .budget
            .lock()
            .expect("upstream budget poisoned")
            .limit = limit;
        // Existing I/O is not aborted. A lower limit blocks new admissions until
        // active calls drain below it; an increase immediately wakes waiters.
        self.0.changed.notify_waiters();
    }
    pub async fn acquire(&self) -> UpstreamPermit {
        loop {
            // notify_waiters wakes even an unpolled Notified created before the
            // state check, so release/update cannot be lost across this check.
            let changed = self.0.changed.notified();
            {
                let mut budget = self.0.budget.lock().expect("upstream budget poisoned");
                if budget.active < budget.limit {
                    budget.active += 1;
                    return UpstreamPermit(self.clone());
                }
            }
            changed.await;
        }
    }
}
pub struct UpstreamPermit(UpstreamLimit);
impl Drop for UpstreamPermit {
    fn drop(&mut self) {
        self.0
            .0
            .budget
            .lock()
            .expect("upstream budget poisoned")
            .active -= 1;
        self.0.0.changed.notify_waiters();
    }
}

#[derive(Clone)]
pub struct ParseSettings {
    storage: Storage,
    upstream: UpstreamLimit,
    updates: Arc<AsyncMutex<()>>,
}
impl ParseSettings {
    pub async fn new(storage: Storage) -> Result<Self> {
        let concurrency = match storage.get(KEY).await? {
            None => DEFAULT_CONCURRENCY,
            Some(value) => value["concurrency"]
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .filter(|n| (1..=MAX_CONCURRENCY).contains(n))
                .ok_or(StorageError::InvalidJson)?,
        };
        Ok(Self {
            storage,
            upstream: UpstreamLimit::new(concurrency),
            updates: Arc::new(AsyncMutex::new(())),
        })
    }
    pub fn upstream(&self) -> UpstreamLimit {
        self.upstream.clone()
    }
    pub fn concurrency(&self) -> usize {
        self.upstream.concurrency()
    }
    pub fn to_json(&self) -> Value {
        json!({"concurrency":self.concurrency(),"default_concurrency":DEFAULT_CONCURRENCY,
            "min_concurrency":1,"max_concurrency":MAX_CONCURRENCY})
    }
    pub async fn set_concurrency(&self, concurrency: usize) -> Result<()> {
        if !(1..=MAX_CONCURRENCY).contains(&concurrency) {
            return Err(StorageError::InvalidCapacity);
        }
        // The accepted SQLite command owns the update guard and publication.
        // Cancelling the HTTP waiter cannot leave a committed setting unpublished
        // or allow a later update to overtake it. Failed writes never publish.
        let update = self.updates.clone().lock_owned().await;
        let upstream = self.upstream.clone();
        let value = encrypt_for_kv(&json!({"concurrency":concurrency}))?;
        self.storage
            .with_connection(move |db| {
                let _update = update;
                db.execute(
                    "INSERT OR REPLACE INTO kv_store(key,value,expires_at) VALUES (?1,?2,0)",
                    rusqlite::params![KEY, value],
                )
                .map_err(|_| StorageError::Database)?;
                upstream.set(concurrency);
                Ok(())
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::time::timeout;

    #[tokio::test]
    async fn settings_default_persistence_validation_and_failed_write() {
        let storage = Storage::open(":memory:", 8).await.unwrap();
        let settings = ParseSettings::new(storage.clone()).await.unwrap();
        assert_eq!(settings.concurrency(), 16);
        assert_eq!(
            storage.get(KEY).await.unwrap(),
            None,
            "default must not mutate DB"
        );
        for invalid in [0, 17, usize::MAX] {
            assert!(settings.set_concurrency(invalid).await.is_err());
            assert_eq!(settings.concurrency(), 16);
        }
        settings.set_concurrency(7).await.unwrap();
        assert_eq!(
            ParseSettings::new(storage.clone())
                .await
                .unwrap()
                .concurrency(),
            7
        );
        storage.close().await.unwrap();
        assert!(settings.set_concurrency(3).await.is_err());
        assert_eq!(settings.concurrency(), 7);
    }
    #[tokio::test]
    async fn saved_concurrency_wins_over_new_default_without_rewriting_kv() {
        for saved in [1, 4, 7, 16] {
            let storage = Storage::open(":memory:", 8).await.unwrap();
            storage
                .put(KEY, json!({"concurrency":saved}), None)
                .await
                .unwrap();
            let before: (String, i64) = storage
                .with_connection(|db| {
                    db.query_row(
                        "SELECT value,expires_at FROM kv_store WHERE key='rust_parse_settings'",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .map_err(|_| StorageError::Database)
                })
                .await
                .unwrap();
            let settings = ParseSettings::new(storage.clone()).await.unwrap();
            assert_eq!(settings.concurrency(), saved);
            assert_eq!(settings.to_json()["default_concurrency"], 16);
            let after: (String, i64) = storage
                .with_connection(|db| {
                    db.query_row(
                        "SELECT value,expires_at FROM kv_store WHERE key='rust_parse_settings'",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .map_err(|_| StorageError::Database)
                })
                .await
                .unwrap();
            assert_eq!(
                before, after,
                "initialization must preserve saved value and TTL"
            );
            storage.close().await.unwrap();
        }
    }

    #[tokio::test]
    async fn global_budget_is_shared_live_resizable_and_cancel_safe() {
        let limit = UpstreamLimit::new(2);
        let a = limit.acquire().await;
        let b = limit.clone().acquire().await;
        assert!(
            timeout(Duration::from_millis(10), limit.acquire())
                .await
                .is_err()
        );
        limit.set(1);
        drop(a);
        assert!(
            timeout(Duration::from_millis(10), limit.acquire())
                .await
                .is_err()
        );
        drop(b);
        let c = timeout(Duration::from_secs(1), limit.acquire())
            .await
            .unwrap();
        let copy = limit.clone();
        let waiter = tokio::spawn(async move { copy.acquire().await });
        tokio::task::yield_now().await;
        limit.set(2);
        let d = timeout(Duration::from_secs(1), waiter)
            .await
            .unwrap()
            .unwrap();
        drop((c, d));
        assert_eq!(limit.0.budget.lock().unwrap().active, 0);
        let held = limit.acquire().await;
        drop(held);
        assert_eq!(limit.0.budget.lock().unwrap().active, 0);
    }

    #[tokio::test]
    async fn accepted_settings_update_publishes_even_if_http_waiter_is_cancelled() {
        use futures_util::poll;
        let storage = Storage::open(":memory:", 8).await.unwrap();
        let settings = ParseSettings::new(storage.clone()).await.unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let actor = storage.clone();
        let blocker = tokio::spawn(async move {
            actor.with_connection(move |db| {
                db.execute_batch("CREATE TABLE settings_test_log(value TEXT); CREATE TRIGGER settings_test_log_insert AFTER INSERT ON kv_store WHEN NEW.key='rust_parse_settings' BEGIN INSERT INTO settings_test_log VALUES(NEW.value); END;")
                    .map_err(|_| StorageError::Database)?;
                started_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                Ok(())
            }).await.unwrap();
        });
        started_rx.await.unwrap();
        let mut cancelled = Box::pin(settings.set_concurrency(7));
        assert!(poll!(cancelled.as_mut()).is_pending());
        drop(cancelled); // command already accepted, not an aborted queued write
        let mut next = Box::pin(settings.set_concurrency(3));
        assert!(poll!(next.as_mut()).is_pending()); // must wait for publication, not overtake it
        assert_eq!(settings.concurrency(), 16);
        release_tx.send(()).unwrap();
        timeout(Duration::from_secs(2), next)
            .await
            .unwrap()
            .unwrap();
        blocker.await.unwrap();
        assert_eq!(settings.concurrency(), 3);
        assert_eq!(
            storage.get(KEY).await.unwrap(),
            Some(json!({"concurrency":3}))
        );
        let log: Vec<String> = storage
            .with_connection(|db| {
                let mut query = db
                    .prepare("SELECT value FROM settings_test_log ORDER BY rowid")
                    .map_err(|_| StorageError::Database)?;
                query
                    .query_map([], |row| row.get(0))
                    .map_err(|_| StorageError::Database)?
                    .collect::<std::result::Result<_, _>>()
                    .map_err(|_| StorageError::Database)
            })
            .await
            .unwrap();
        let values: Vec<_> = log
            .iter()
            .map(|v| crate::storage::decrypt_from_kv(v).unwrap())
            .collect();
        assert_eq!(values, [json!({"concurrency":7}), json!({"concurrency":3})]);
        storage.close().await.unwrap();
    }

    #[tokio::test]
    async fn cloud_and_mobile_share_budget_until_body_finishes_or_is_cancelled() {
        use crate::providers::{cloud189_client::Cloud189Client, mcloud_client::MobileCloudClient};
        use futures_util::poll;
        use std::{
            io::{Read, Write},
            net::TcpListener,
            sync::atomic::{AtomicUsize, Ordering},
        };
        // Only synthetic loopback traffic; the cloud body deliberately stalls
        // after headers. Mobile must not reach its separate server meanwhile.
        fn consume_request(stream: &mut std::net::TcpStream) {
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut bytes = Vec::new();
            let header_end = loop {
                let mut buf = [0; 1024];
                let n = stream.read(&mut buf).unwrap();
                assert!(n > 0 && bytes.len() < 16 * 1024);
                bytes.extend_from_slice(&buf[..n]);
                if let Some(i) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    break i + 4;
                }
            };
            let length = std::str::from_utf8(&bytes[..header_end])
                .unwrap()
                .lines()
                .find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            while bytes.len() < header_end + length {
                let mut buf = [0; 1024];
                let n = stream.read(&mut buf).unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buf[..n]);
            }
        }
        let storage = Storage::open(":memory:", 8).await.unwrap();
        let settings = ParseSettings::new(storage.clone()).await.unwrap();
        settings.set_concurrency(1).await.unwrap();
        let cloud_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let cloud_base = format!("http://{}/", cloud_listener.local_addr().unwrap());
        let mobile_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mobile_address = mobile_listener.local_addr().unwrap();
        let (headers_tx, headers_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let cloud_server = std::thread::spawn(move || {
            let (mut stream, _) = cloud_listener.accept().unwrap();
            consume_request(&mut stream);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 64\r\nConnection: close\r\n\r\n{")
                .unwrap();
            headers_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        });
        let mobile_calls = Arc::new(AtomicUsize::new(0));
        let calls = mobile_calls.clone();
        let mobile_server = std::thread::spawn(move || {
            let (mut stream, _) = mobile_listener.accept().unwrap();
            consume_request(&mut stream);
            calls.fetch_add(1, Ordering::SeqCst);
            let body = r#"{"success":true,"data":{"redrUrl":"https://cdn.invalid/synthetic"}}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let mut cloud = Cloud189Client::for_loopback_test(http.clone(), json!({}), &cloud_base)
            .with_limit(settings.upstream());
        let mobile = MobileCloudClient::new_for_loopback(http, String::new(), mobile_address)
            .unwrap()
            .with_limit(settings.upstream());
        assert_eq!(cloud.concurrency(), 1);
        assert_eq!(mobile.concurrency(), 1);
        let cloud_task = tokio::spawn(async move { cloud.get_share_info("synthetic").await });
        timeout(Duration::from_secs(2), headers_rx)
            .await
            .unwrap()
            .unwrap();
        let mut waiting = Box::pin(mobile.download_url("synthetic", "file"));
        assert!(poll!(waiting.as_mut()).is_pending());
        assert_eq!(
            mobile_calls.load(Ordering::SeqCst),
            0,
            "permit cannot end at response headers"
        );
        assert_eq!(settings.upstream.0.budget.lock().unwrap().active, 1);
        cloud_task.abort();
        assert!(cloud_task.await.unwrap_err().is_cancelled());
        assert_eq!(
            timeout(Duration::from_secs(2), waiting)
                .await
                .unwrap()
                .unwrap()
                .as_deref(),
            Some("https://cdn.invalid/synthetic")
        );
        assert_eq!(mobile_calls.load(Ordering::SeqCst), 1);
        assert_eq!(settings.upstream.0.budget.lock().unwrap().active, 0);
        release_tx.send(()).unwrap();
        cloud_server.join().unwrap();
        mobile_server.join().unwrap();
        storage.close().await.unwrap();
    }
}
