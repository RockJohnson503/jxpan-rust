use std::{
    future::{Future, pending},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use futures_util::{FutureExt, future, task::noop_waker_ref};
use jxpan_rust::cache::{BoundedTtlCache, CacheConfig, CacheError, CacheStats};

fn config() -> CacheConfig {
    CacheConfig {
        ttl: Duration::from_secs(60),
        max_entries: 4,
        max_bytes: 1024,
        max_key_bytes: 16,
        max_in_flight: 2,
    }
}

fn poll_once<F: Future>(future: std::pin::Pin<&mut F>) -> Poll<F::Output> {
    future.poll(&mut Context::from_waker(noop_waker_ref()))
}

#[tokio::test]
async fn successful_load_is_shared_by_arc_and_retained() {
    let cache = BoundedTtlCache::new(config());
    let calls = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&calls);
    let a = cache
        .get_or_init(
            "a".into(),
            move || async move {
                count.fetch_add(1, Ordering::SeqCst);
                "value".to_owned()
            },
            |v| Some(v.len()),
        )
        .await
        .unwrap();
    let b = cache
        .get_or_init("a".into(), || async { panic!("cache miss") }, |_| Some(5))
        .await
        .unwrap();
    assert!(Arc::ptr_eq(&a, &b));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        cache.stats(),
        CacheStats {
            entries: 1,
            bytes: 6,
            in_flight: 0
        }
    );
    assert!(cache.invalidate("a"));
    assert!(cache.get("a").is_none());
    assert!(!cache.invalidate("a"));
}

#[tokio::test]
async fn concurrent_duplicate_loads_run_once_and_different_keys_progress() {
    let cache = BoundedTtlCache::new(config());
    let calls = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&calls);
    // A manually polled gate avoids sleeps/timing dependencies and exercises overlap.
    let gate = Arc::new(AtomicUsize::new(0));
    let load_gate = Arc::clone(&gate);
    let mut first = Box::pin(cache.get_or_init(
        "a".into(),
        move || async move {
            count.fetch_add(1, Ordering::SeqCst);
            future::poll_fn(|_| {
                if load_gate.load(Ordering::SeqCst) == 1 {
                    Poll::Ready(7)
                } else {
                    Poll::Pending
                }
            })
            .await
        },
        |_| Some(1),
    ));
    assert!(poll_once(first.as_mut()).is_pending());
    let mut duplicates: Vec<_> = (0..100)
        .map(|_| {
            Box::pin(cache.get_or_init(
                "a".into(),
                || async { panic!("duplicate loader invoked") },
                |_| Some(1),
            ))
        })
        .collect();
    for duplicate in &mut duplicates {
        assert!(poll_once(duplicate.as_mut()).is_pending());
    }
    assert_eq!(cache.stats().in_flight, 1);
    let other = cache
        .get_or_init("b".into(), || async { 9 }, |_| Some(1))
        .await
        .unwrap();
    assert_eq!(*other, 9);
    gate.store(1, Ordering::SeqCst);
    let value = match poll_once(first.as_mut()) {
        Poll::Ready(Ok(value)) => value,
        _ => panic!("not ready"),
    };
    for mut duplicate in duplicates {
        let Poll::Ready(Ok(other)) = poll_once(duplicate.as_mut()) else {
            panic!("not ready")
        };
        assert!(Arc::ptr_eq(&value, &other));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(cache.stats().in_flight, 0);
}

#[tokio::test]
async fn cancelling_leader_does_not_cancel_remaining_waiter() {
    let cache = BoundedTtlCache::new(config());
    let gate = Arc::new(AtomicUsize::new(0));
    let load_gate = Arc::clone(&gate);
    let mut leader = Box::pin(cache.get_or_init(
        "a".into(),
        move || async move {
            future::poll_fn(|_| {
                if load_gate.load(Ordering::SeqCst) == 1 {
                    Poll::Ready(7)
                } else {
                    Poll::Pending
                }
            })
            .await
        },
        |_| Some(1),
    ));
    assert!(poll_once(leader.as_mut()).is_pending());
    let mut waiter =
        Box::pin(cache.get_or_init("a".into(), || async { panic!("duplicate") }, |_| Some(1)));
    assert!(poll_once(waiter.as_mut()).is_pending());
    drop(leader);
    assert_eq!(cache.stats().in_flight, 1);
    gate.store(1, Ordering::SeqCst);
    assert_eq!(*waiter.await.unwrap(), 7);
    assert_eq!(cache.stats().in_flight, 0);
}

#[tokio::test]
async fn all_waiters_cancel_releases_load_and_key_slot() {
    let cache = BoundedTtlCache::new(config());
    let retained = Arc::new(());
    let owned = Arc::clone(&retained);
    let mut load = Box::pin(cache.get_or_init(
        "a".into(),
        move || async move {
            let _owned = owned;
            pending::<u8>().await
        },
        |_| Some(1),
    ));
    assert!(poll_once(load.as_mut()).is_pending());
    assert_eq!(Arc::strong_count(&retained), 2);
    drop(load);
    assert_eq!(Arc::strong_count(&retained), 1);
    assert_eq!(cache.stats(), CacheStats::default());
    let retry = cache
        .get_or_init("a".into(), || async { 1 }, |_| Some(1))
        .await
        .unwrap();
    assert_eq!(*retry, 1);
}

#[tokio::test]
async fn active_loads_and_keys_have_explicit_admission_bounds() {
    let cache = BoundedTtlCache::<u8>::new(config());
    let mut a = Box::pin(cache.get_or_init("a".into(), pending, |_| Some(1)));
    let mut b = Box::pin(cache.get_or_init("b".into(), pending, |_| Some(1)));
    assert!(poll_once(a.as_mut()).is_pending());
    assert!(poll_once(b.as_mut()).is_pending());
    assert_eq!(
        cache
            .get_or_init(
                "c".into(),
                || async { panic!("must not load") },
                |_| Some(1)
            )
            .await,
        Err(CacheError::InFlightLimit)
    );
    let mut duplicate = Box::pin(cache.get_or_init("a".into(), pending, |_| Some(1)));
    assert!(poll_once(duplicate.as_mut()).is_pending());
    assert_eq!(cache.stats().in_flight, 2);
    assert_eq!(
        cache
            .get_or_init(
                "x".repeat(17),
                || async { panic!("must not load") },
                |_| Some(1)
            )
            .await,
        Err(CacheError::KeyTooLarge)
    );
    // Invalidating/clearing cannot bypass admission with still-running old loads.
    assert!(cache.invalidate("a"));
    cache.clear();
    assert_eq!(cache.stats().in_flight, 2);
    assert_eq!(
        cache
            .get_or_init("c".into(), || async { 1 }, |_| Some(1))
            .await,
        Err(CacheError::InFlightLimit)
    );
    drop((a, b, duplicate));
    assert_eq!(cache.stats(), CacheStats::default());
}

#[tokio::test]
async fn invalidated_old_completion_cannot_remove_or_overwrite_new_generation() {
    let cache = BoundedTtlCache::new(config());
    let gate = Arc::new(AtomicUsize::new(0));
    let old_gate = Arc::clone(&gate);
    let mut old = Box::pin(cache.get_or_init(
        "a".into(),
        move || async move {
            future::poll_fn(|_| {
                if old_gate.load(Ordering::SeqCst) == 1 {
                    Poll::Ready(1)
                } else {
                    Poll::Pending
                }
            })
            .await
        },
        |_| Some(1),
    ));
    assert!(poll_once(old.as_mut()).is_pending());
    assert!(cache.invalidate("a"));
    let new = cache
        .get_or_init("a".into(), || async { 2 }, |_| Some(1))
        .await
        .unwrap();
    gate.store(1, Ordering::SeqCst);
    assert_eq!(*old.await.unwrap(), 1);
    assert!(Arc::ptr_eq(&cache.get("a").unwrap(), &new));
    assert_eq!(cache.stats().in_flight, 0);
}

#[tokio::test]
async fn failures_and_oversized_results_are_shared_but_not_retained() {
    let cache = BoundedTtlCache::new(config());
    let calls = Arc::new(AtomicUsize::new(0));
    for _ in 0..2 {
        let count = Arc::clone(&calls);
        assert_eq!(
            *cache
                .get_or_init(
                    "a".into(),
                    move || async move {
                        count.fetch_add(1, Ordering::SeqCst);
                        Err::<u8, &str>("upstream")
                    },
                    |result| result.as_ref().ok().map(|_| 1)
                )
                .await
                .unwrap(),
            Err("upstream")
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(cache.stats(), CacheStats::default());
    let oversized = cache
        .get_or_init("a".into(), || async { Ok(1) }, |_| Some(1024))
        .await
        .unwrap();
    assert_eq!(*oversized, Ok(1));
    assert_eq!(cache.stats(), CacheStats::default());
}

#[tokio::test]
async fn disabled_cache_still_coalesces_without_retention() {
    let cache = BoundedTtlCache::new(CacheConfig {
        ttl: Duration::ZERO,
        ..config()
    });
    cache
        .get_or_init("a".into(), || async { 1 }, |_| Some(1))
        .await
        .unwrap();
    assert_eq!(cache.stats(), CacheStats::default());
    let cache = BoundedTtlCache::new(CacheConfig {
        max_entries: 0,
        ..config()
    });
    cache
        .get_or_init("a".into(), || async { 1 }, |_| Some(1))
        .await
        .unwrap();
    assert_eq!(cache.stats(), CacheStats::default());
    let cache = BoundedTtlCache::new(CacheConfig {
        max_in_flight: 0,
        ..config()
    });
    assert_eq!(
        cache
            .get_or_init("a".into(), || async { 1 }, |_| Some(1))
            .await,
        Err(CacheError::InFlightLimit)
    );
}

#[test]
fn same_key_coalesces_across_os_threads() {
    const WORKERS: usize = 32;
    let cache = BoundedTtlCache::new(config());
    let calls = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(AtomicUsize::new(0));
    let all_polled = Arc::new(std::sync::Barrier::new(WORKERS));
    let all_released = Arc::new(std::sync::Barrier::new(WORKERS));
    let values = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..WORKERS)
            .map(|id| {
                let cache = cache.clone();
                let calls = Arc::clone(&calls);
                let gate = Arc::clone(&gate);
                let all_polled = Arc::clone(&all_polled);
                let all_released = Arc::clone(&all_released);
                scope.spawn(move || {
                    let load_gate = Arc::clone(&gate);
                    let mut request = Box::pin(cache.get_or_init(
                        "a".into(),
                        move || async move {
                            calls.fetch_add(1, Ordering::SeqCst);
                            future::poll_fn(|_| {
                                if load_gate.load(Ordering::SeqCst) == 1 {
                                    Poll::Ready(7)
                                } else {
                                    Poll::Pending
                                }
                            })
                            .await
                        },
                        |_| Some(1),
                    ));
                    assert!(poll_once(request.as_mut()).is_pending());
                    all_polled.wait();
                    if id == 0 {
                        gate.store(1, Ordering::SeqCst);
                    }
                    all_released.wait();
                    let deadline = std::time::Instant::now() + Duration::from_secs(5);
                    loop {
                        if let Poll::Ready(result) = poll_once(request.as_mut()) {
                            break result.unwrap();
                        }
                        assert!(
                            std::time::Instant::now() < deadline,
                            "load did not complete"
                        );
                        std::thread::yield_now();
                    }
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(values.iter().all(|value| Arc::ptr_eq(value, &values[0])));
    assert_eq!(cache.stats().in_flight, 0);
}

#[tokio::test]
async fn concurrent_errors_are_shared_and_next_request_retries() {
    let cache = BoundedTtlCache::new(config());
    let gate = Arc::new(AtomicUsize::new(0));
    let load_gate = Arc::clone(&gate);
    let mut first = Box::pin(cache.get_or_init(
        "a".into(),
        move || async move {
            future::poll_fn(|_| {
                if load_gate.load(Ordering::SeqCst) == 1 {
                    Poll::Ready(Err::<u8, _>("upstream"))
                } else {
                    Poll::Pending
                }
            })
            .await
        },
        |_| None,
    ));
    assert!(poll_once(first.as_mut()).is_pending());
    let mut duplicate =
        Box::pin(cache.get_or_init("a".into(), || async { panic!("duplicate") }, |_| Some(1)));
    assert!(poll_once(duplicate.as_mut()).is_pending());
    gate.store(1, Ordering::SeqCst);
    let a = first.await.unwrap();
    let b = duplicate.await.unwrap();
    assert!(Arc::ptr_eq(&a, &b));
    assert_eq!(cache.stats(), CacheStats::default());
    let retry = cache
        .get_or_init("a".into(), || async { Ok(1) }, |_| Some(1))
        .await
        .unwrap();
    assert_eq!(*retry, Ok(1));
}

#[tokio::test]
async fn old_completion_leaves_replacement_flight_available_for_coalescing() {
    let cache = BoundedTtlCache::new(config());
    let old_gate = Arc::new(AtomicUsize::new(0));
    let new_gate = Arc::new(AtomicUsize::new(0));
    let gate = Arc::clone(&old_gate);
    let mut old = Box::pin(cache.get_or_init(
        "a".into(),
        move || async move {
            future::poll_fn(|_| {
                if gate.load(Ordering::SeqCst) == 1 {
                    Poll::Ready(1)
                } else {
                    Poll::Pending
                }
            })
            .await
        },
        |_| Some(1),
    ));
    assert!(poll_once(old.as_mut()).is_pending());
    cache.clear();
    let gate = Arc::clone(&new_gate);
    let mut new = Box::pin(cache.get_or_init(
        "a".into(),
        move || async move {
            future::poll_fn(|_| {
                if gate.load(Ordering::SeqCst) == 1 {
                    Poll::Ready(2)
                } else {
                    Poll::Pending
                }
            })
            .await
        },
        |_| Some(1),
    ));
    assert!(poll_once(new.as_mut()).is_pending());
    assert_eq!(cache.stats().in_flight, 2);
    old_gate.store(1, Ordering::SeqCst);
    assert_eq!(*old.await.unwrap(), 1);
    assert!(cache.get("a").is_none());
    assert_eq!(cache.stats().in_flight, 1);
    let mut duplicate = Box::pin(cache.get_or_init(
        "a".into(),
        || async { panic!("replacement was detached") },
        |_| Some(1),
    ));
    assert!(poll_once(duplicate.as_mut()).is_pending());
    new_gate.store(1, Ordering::SeqCst);
    let a = new.await.unwrap();
    let b = duplicate.await.unwrap();
    assert!(Arc::ptr_eq(&a, &b));
    assert_eq!(*cache.get("a").unwrap(), 2);
    assert_eq!(cache.stats().in_flight, 0);
}

#[tokio::test]
async fn panic_does_not_poison_cache_or_leave_live_flight() {
    let cache = BoundedTtlCache::<u8>::new(config());
    let result = std::panic::AssertUnwindSafe(cache.get_or_init(
        "a".into(),
        || async { panic!("load panic") },
        |_| Some(1),
    ))
    .catch_unwind()
    .await;
    assert!(result.is_err());
    assert_eq!(cache.stats(), CacheStats::default());
    assert_eq!(
        *cache
            .get_or_init("a".into(), || async { 1 }, |_| Some(1))
            .await
            .unwrap(),
        1
    );
}
