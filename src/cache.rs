//! Bounded, process-local TTL caching and cancellation-safe request coalescing.
//!
//! Keys must include every input affecting the response, including credential scope.
//! Values are shared by `Arc`; callers supply a conservative retained-size estimate.
//! Expired values are reclaimed on the next cache operation (no background task).

use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex, Weak},
    time::{Duration, Instant},
};

use futures_util::future::{BoxFuture, FutureExt, Shared};

#[derive(Clone, Copy, Debug)]
pub struct CacheConfig {
    pub ttl: Duration,
    pub max_entries: usize,
    /// Budget for ready-value weights plus their keys, not exact allocator/RSS usage.
    pub max_bytes: usize,
    pub max_key_bytes: usize,
    /// Maximum distinct active loads. Duplicate requests do not consume extra slots.
    pub max_in_flight: usize,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(60),
            max_entries: 256,
            max_bytes: 16 * 1024 * 1024,
            max_key_bytes: 16 * 1024,
            max_in_flight: 128,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheError {
    KeyTooLarge,
    InFlightLimit,
}

impl std::fmt::Display for CacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::KeyTooLarge => "cache key exceeds size limit",
            Self::InFlightLimit => "cache concurrent-load limit reached",
        })
    }
}

impl std::error::Error for CacheError {}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CacheStats {
    pub entries: usize,
    pub bytes: usize,
    pub in_flight: usize,
}

/// How a caller obtained its response; a joined failure is not a successful cache hit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheSource {
    Hit,
    Joined,
    Loaded,
}

/// Clones share the same cache and in-flight registry. No locks span an await.
pub struct BoundedTtlCache<V> {
    inner: Arc<Inner<V>>,
}

impl<V> Clone for BoundedTtlCache<V> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

struct Inner<V> {
    config: CacheConfig,
    state: Mutex<State<V>>,
}

struct State<V> {
    entries: HashMap<String, Entry<V>>,
    // Weak references let cancellation of ALL callers immediately drop the load.
    // Dead keys are swept on every operation and never exceed max_in_flight.
    flights: HashMap<String, Weak<Flight<V>>>,
    // Detached (invalidated) loads still consume concurrency capacity.
    active: Vec<Weak<Flight<V>>>,
    bytes: usize,
}

struct Entry<V> {
    value: Arc<V>,
    expires_at: Instant,
    accessed_at: Instant,
    weight: usize,
}

struct Flight<V> {
    identity: Arc<()>,
    future: Shared<BoxFuture<'static, Arc<V>>>,
}

impl<V> State<V> {
    fn prune(&mut self, now: Instant) {
        self.entries.retain(|_, entry| {
            if entry.expires_at <= now {
                self.bytes -= entry.weight;
                false
            } else {
                true
            }
        });
        self.flights.retain(|_, flight| flight.strong_count() > 0);
        self.active.retain(|flight| flight.strong_count() > 0);
    }

    fn remove_entry(&mut self, key: &str) -> bool {
        if let Some(entry) = self.entries.remove(key) {
            self.bytes -= entry.weight;
            true
        } else {
            false
        }
    }

    fn insert(
        &mut self,
        config: CacheConfig,
        key: String,
        value: Arc<V>,
        value_weight: usize,
        now: Instant,
    ) {
        self.remove_entry(&key);
        let Some(weight) = key.len().checked_add(value_weight) else {
            return;
        };
        let Some(expires_at) = now.checked_add(config.ttl) else {
            return;
        };
        if config.ttl.is_zero() || config.max_entries == 0 || weight > config.max_bytes {
            return;
        }
        while self.entries.len() >= config.max_entries || self.bytes > config.max_bytes - weight {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.accessed_at)
                .map(|(key, _)| key.clone())
            else {
                return;
            };
            self.remove_entry(&oldest);
        }
        self.bytes += weight;
        self.entries.insert(
            key,
            Entry {
                value,
                expires_at,
                accessed_at: now,
                weight,
            },
        );
    }
}

impl<V: Send + Sync + 'static> BoundedTtlCache<V> {
    pub fn new(config: CacheConfig) -> Self {
        Self {
            inner: Arc::new(Inner {
                config,
                state: Mutex::new(State {
                    entries: HashMap::new(),
                    flights: HashMap::new(),
                    active: Vec::new(),
                    bytes: 0,
                }),
            }),
        }
    }

    pub fn get(&self, key: &str) -> Option<Arc<V>> {
        let now = Instant::now();
        let mut state = self.inner.state.lock().expect("cache mutex poisoned");
        state.prune(now);
        let entry = state.entries.get_mut(key)?;
        entry.accessed_at = now;
        Some(Arc::clone(&entry.value))
    }

    /// Coalesce identical loads, then cache only when `cache_weight` returns Some.
    /// Return None for upstream errors/negative responses; concurrent waiters still
    /// share that result, but the next request retries. Oversize results are also
    /// shared without retention. TTL starts at completion, never at request start.
    ///
    /// Dropping one waiter does not cancel others. Dropping all waiters cancels
    /// the underlying future without leaving a live flight or a background task.
    pub async fn get_or_init<F, Fut, W>(
        &self,
        key: String,
        load: F,
        cache_weight: W,
    ) -> Result<Arc<V>, CacheError>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = V> + Send + 'static,
        W: FnOnce(&V) -> Option<usize> + Send + 'static,
    {
        self.get_or_init_with_source(key, load, cache_weight)
            .await
            .map(|(value, _)| value)
    }

    /// The same bounded loader, with provenance for request statistics.
    pub async fn get_or_init_with_source<F, Fut, W>(
        &self,
        key: String,
        load: F,
        cache_weight: W,
    ) -> Result<(Arc<V>, CacheSource), CacheError>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = V> + Send + 'static,
        W: FnOnce(&V) -> Option<usize> + Send + 'static,
    {
        if key.len() > self.inner.config.max_key_bytes {
            return Err(CacheError::KeyTooLarge);
        }
        // A short key with a huge reserved String capacity must not retain that buffer.
        let key = key.into_boxed_str().into_string();
        let (flight, source) = {
            let now = Instant::now();
            let mut state = self.inner.state.lock().expect("cache mutex poisoned");
            state.prune(now);
            if let Some(entry) = state.entries.get_mut(&key) {
                entry.accessed_at = now;
                return Ok((Arc::clone(&entry.value), CacheSource::Hit));
            }
            if let Some(flight) = state.flights.get(&key).and_then(Weak::upgrade) {
                (flight, CacheSource::Joined)
            } else {
                if state.active.len() >= self.inner.config.max_in_flight {
                    return Err(CacheError::InFlightLimit);
                }
                let identity = Arc::new(());
                let completion_identity = Arc::clone(&identity);
                let inner = Arc::downgrade(&self.inner);
                let completion_key = key.clone();
                let future = async move {
                    let value = Arc::new(load().await);
                    let weight = cache_weight(&value);
                    if let Some(inner) = inner.upgrade() {
                        let now = Instant::now();
                        let mut state = inner.state.lock().expect("cache mutex poisoned");
                        state.prune(now);
                        state.active.retain(|flight| {
                            flight.upgrade().is_some_and(|flight| {
                                !Arc::ptr_eq(&flight.identity, &completion_identity)
                            })
                        });
                        let current = state
                            .flights
                            .get(&completion_key)
                            .and_then(Weak::upgrade)
                            .is_some_and(|flight| {
                                Arc::ptr_eq(&flight.identity, &completion_identity)
                            });
                        // An invalidated/replaced load must never repopulate the cache.
                        if current {
                            state.flights.remove(&completion_key);
                            if let Some(weight) = weight {
                                state.insert(
                                    inner.config,
                                    completion_key,
                                    Arc::clone(&value),
                                    weight,
                                    now,
                                );
                            }
                        }
                    }
                    value
                }
                .boxed()
                .shared();
                let flight = Arc::new(Flight { identity, future });
                state.flights.insert(key, Arc::downgrade(&flight));
                state.active.push(Arc::downgrade(&flight));
                (flight, CacheSource::Loaded)
            }
        };
        let value = flight.future.clone().await;
        drop(flight);
        Ok((value, source))
    }

    /// Detach any active load. Existing waiters still receive its result, but it
    /// cannot cache that result and subsequent requests start a new generation.
    pub fn invalidate(&self, key: &str) -> bool {
        let mut state = self.inner.state.lock().expect("cache mutex poisoned");
        state.prune(Instant::now());
        let removed_entry = state.remove_entry(key);
        let removed_flight = state.flights.remove(key).is_some();
        removed_entry || removed_flight
    }

    pub fn clear(&self) {
        let mut state = self.inner.state.lock().expect("cache mutex poisoned");
        state.entries.clear();
        state.flights.clear();
        state.bytes = 0;
    }

    pub fn stats(&self) -> CacheStats {
        let mut state = self.inner.state.lock().expect("cache mutex poisoned");
        state.prune(Instant::now());
        CacheStats {
            entries: state.entries.len(),
            bytes: state.bytes,
            in_flight: state.active.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn app_download_cache_matches_legacy_ten_minute_ttl_and_budgets() {
        let storage = crate::storage::Storage::open(":memory:", 16).await.unwrap();
        let app = crate::http::AppState::new(storage, serde_json::json!({}), "", "")
            .await
            .unwrap();
        let download = app.download_cache.inner.config;
        assert_eq!(download.ttl, Duration::from_secs(600));
        assert_eq!(download.max_entries, 64);
        assert_eq!(download.max_bytes, 256 * 1024);
        assert_eq!(download.max_key_bytes, 64);
        assert_eq!(download.max_in_flight, 8);
        assert_eq!(app.parse_cache.inner.config.ttl, Duration::from_secs(600));
        assert_eq!(app.parse_cache.inner.config.max_bytes, 8 * 1024 * 1024);
        assert_eq!(app.directories.inner.config.ttl, Duration::from_secs(600));
        assert_eq!(app.directories.inner.config.max_bytes, 256 * 1024);

        // Exercise the actual AppState configuration with a deterministic clock.
        // No real waiting, credential files, HTTP requests, or provider operations.
        let cache = BoundedTtlCache::<u8>::new(download);
        let start = Instant::now();
        {
            let mut state = cache.inner.state.lock().unwrap();
            state.insert(download, "synthetic".into(), Arc::new(1), 1, start);
            state.prune(start + Duration::from_secs(61));
            assert!(state.entries.contains_key("synthetic"));
            state.prune(start + Duration::from_millis(599_999));
            assert!(state.entries.contains_key("synthetic"));
            state.entries.get_mut("synthetic").unwrap().accessed_at =
                start + Duration::from_millis(599_999);
            state.prune(start + Duration::from_secs(600));
            assert!(state.entries.is_empty());
            assert_eq!(state.bytes, 0);
        }
        app.close().await.unwrap();
    }

    fn config() -> CacheConfig {
        CacheConfig {
            ttl: Duration::from_secs(1),
            max_entries: 2,
            max_bytes: 10,
            max_key_bytes: 16,
            max_in_flight: 2,
        }
    }

    #[test]
    fn expiration_is_absolute_and_exact_and_releases_weight() {
        let cache = BoundedTtlCache::<u8>::new(config());
        let start = Instant::now();
        let mut state = cache.inner.state.lock().unwrap();
        state.insert(config(), "a".into(), Arc::new(1), 3, start);
        state.prune(start + Duration::from_millis(999));
        assert_eq!(state.entries.len(), 1);
        state.entries.get_mut("a").unwrap().accessed_at = start + Duration::from_millis(999);
        state.prune(start + Duration::from_secs(1));
        assert!(state.entries.is_empty());
        assert_eq!(state.bytes, 0);
    }

    #[test]
    fn entries_and_bytes_are_bounded_and_eviction_is_lru() {
        let cache = BoundedTtlCache::<u8>::new(config());
        let start = Instant::now();
        let mut state = cache.inner.state.lock().unwrap();
        state.insert(config(), "a".into(), Arc::new(1), 3, start);
        state.insert(
            config(),
            "b".into(),
            Arc::new(2),
            3,
            start + Duration::from_millis(1),
        );
        state.entries.get_mut("a").unwrap().accessed_at = start + Duration::from_millis(2);
        state.insert(
            config(),
            "c".into(),
            Arc::new(3),
            3,
            start + Duration::from_millis(3),
        );
        assert!(state.entries.contains_key("a"));
        assert!(!state.entries.contains_key("b"));
        assert_eq!(state.bytes, 8);
        state.insert(
            config(),
            "d".into(),
            Arc::new(4),
            8,
            start + Duration::from_millis(4),
        );
        assert_eq!(state.entries.len(), 1);
        assert_eq!(state.bytes, 9);
        // Too large must not evict a usable value, and weight overflow is rejected.
        state.insert(config(), "e".into(), Arc::new(5), 10, start);
        state.insert(config(), "f".into(), Arc::new(6), usize::MAX, start);
        assert_eq!(state.entries.len(), 1);
        assert_eq!(state.bytes, 9);
        for n in 0..10_000 {
            state.insert(config(), n.to_string(), Arc::new(1), 2, start);
            assert!(state.entries.len() <= 2);
            assert!(state.bytes <= 10);
        }
        state.prune(start + Duration::from_secs(1));
        assert_eq!(state.bytes, 0);
    }
}
