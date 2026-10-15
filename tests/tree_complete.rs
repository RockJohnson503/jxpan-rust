//! Correctness regressions for the production rolling walker, not legacy JS vectors.
use jxpan_rust::tree::{
    MAX_PRODUCTION_DEPTH, MAX_PRODUCTION_ENTRIES, TreeError, byte_size, nested_tree_complete,
    normalize_entry, walk_complete,
};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::{
    sync::Semaphore,
    time::{Duration, timeout},
};

#[test]
fn metadata_preserves_zero_unknown_large_sizes_and_explicit_types() {
    for (input, expected) in [
        (
            json!({"id":0,"name":"empty","fileSize":0,"size":99,"isFolder":"false","coType":1}),
            json!({"fileId":"0","fileName":"empty","isFolder":false,"fileSize":0}),
        ),
        (
            json!({"id":"u","isDir":"0"}),
            json!({"fileId":"u","fileName":"u","isFolder":false,"fileSize":null}),
        ),
        (
            json!({"id":"big","fileSize":9007199254740993u64}),
            json!({"fileId":"big","fileName":"big","isFolder":false,"fileSize":"9007199254740993"}),
        ),
        (
            json!({"id":"folder","isDirectory":"1","fileSize":0}),
            json!({"fileId":"folder","fileName":"folder","isFolder":true}),
        ),
    ] {
        assert_eq!(normalize_entry(&input).unwrap(), expected);
    }
    assert_eq!(
        byte_size(&json!("18446744073709551615")).unwrap(),
        json!("18446744073709551615")
    );
    for size in [
        json!(-1),
        json!(1.5),
        json!("bad"),
        json!("18446744073709551616"),
        json!(false),
    ] {
        assert_eq!(byte_size(&size), Err(TreeError::InvalidEntry));
    }
    for entry in [
        Value::Null,
        json!({"name":"missing"}),
        json!({"id":false}),
        json!({"id":1.5}),
    ] {
        assert_eq!(normalize_entry(&entry), Err(TreeError::InvalidEntry));
    }
}

#[tokio::test]
async fn collision_projection_never_overwrites_files_folders_or_reserved_keys() {
    let walk = walk_complete(
        |id| async move {
            Ok::<_, ()>(if id == "root" {
                vec![
                    json!({"id":"d","name":"same","isFolder":true}),
                    json!({"id":"f","name":"same","size":0}),
                    json!({"id":"f2","name":"same#f","size":2}),
                    json!({"id":"f3","name":"same#f#f2","size":3}),
                    json!({"id":"p","name":"__proto__"}),
                    json!({"id":"id-dir","name":"id","isFolder":true}),
                    json!({"id":"size-dir","name":"size","isFolder":true}),
                ]
            } else {
                vec![]
            })
        },
        "root",
        "share",
        4,
    )
    .await
    .unwrap();
    let projected = nested_tree_complete(&walk, "root", "share");
    let root = projected["share"].as_object().unwrap();
    assert_eq!(root.len(), 7);
    let files: Vec<_> = root.values().filter_map(|v| v["id"].as_str()).collect();
    assert_eq!(files.len(), 4);
    for id in ["f", "f2", "f3", "p"] {
        assert!(files.contains(&id));
    }
    assert_eq!(root["same#f"]["size"], 0);
    assert_eq!(root["__proto__"]["size"], Value::Null);
    assert!(root["id"].is_object() && root["size"].is_object());
}

#[tokio::test]
async fn identities_cannot_collide_across_directories_types_or_root() {
    for scenario in 0..4 {
        for concurrency in [1, 2, 4, 16] {
            let result = walk_complete(
                move |id| async move {
                    Ok::<_, ()>(match id.as_str() {
                        "root" => vec![
                            json!({"id":"a","isFolder":true}),
                            json!({"id":"b","isFolder":true}),
                        ],
                        "a" => vec![match scenario {
                            0 => json!({"id":"b","fileSize":1}),
                            1 => json!({"id":"root","fileSize":1}),
                            _ => json!({"id":"f","fileSize":1}),
                        }],
                        "b" if scenario >= 2 => {
                            vec![json!({"id":"f","fileSize":if scenario == 2 {2} else {1}})]
                        }
                        _ => vec![],
                    })
                },
                "root",
                "",
                concurrency,
            )
            .await;
            assert!(
                matches!(result, Err(TreeError::InvalidEntry | TreeError::Cycle)),
                "duplicate ID must never choose metadata by completion order"
            );
        }
    }
}

#[tokio::test]
async fn upstream_failure_cycle_depth_and_resource_limits_are_explicit_errors() {
    let failure = walk_complete(|_| async { Err::<Vec<Value>, _>(()) }, "root", "", 4).await;
    assert!(matches!(failure, Err(TreeError::Upstream)));
    let cycle = walk_complete(
        |_| async { Ok::<_, ()>(vec![json!({"id":"root","isFolder":true})]) },
        "root",
        "",
        4,
    )
    .await;
    assert!(matches!(cycle, Err(TreeError::Cycle)));
    for depth in [MAX_PRODUCTION_DEPTH, MAX_PRODUCTION_DEPTH + 1] {
        let result = walk_complete(
            move |id| async move {
                let n = id.parse::<usize>().unwrap();
                Ok::<_, ()>(if n + 1 < depth {
                    vec![json!({"id":(n+1).to_string(),"isFolder":true})]
                } else {
                    vec![json!({"id":"file","fileSize":0})]
                })
            },
            "0",
            "",
            16,
        )
        .await;
        if depth == MAX_PRODUCTION_DEPTH {
            assert_eq!(result.unwrap().visited, depth);
        } else {
            assert!(matches!(result, Err(TreeError::Limit)));
        }
    }
    let many = walk_complete(
        |_| async {
            Ok::<_, ()>(
                (0..=MAX_PRODUCTION_ENTRIES)
                    .map(|n| json!({"id":format!("f-{n}")}))
                    .collect(),
            )
        },
        "root",
        "",
        4,
    )
    .await;
    assert!(
        matches!(many, Err(TreeError::Limit)),
        "no safety ceiling may produce partial success"
    );
}

struct Active(Arc<AtomicUsize>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn rolling_refills_before_slow_siblings_finish_at_every_parallel_limit() {
    for concurrency in [2, 4, 16] {
        let descendant = Arc::new(Semaphore::new(0));
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let tree = timeout(
            Duration::from_secs(2),
            walk_complete(
                {
                    let descendant = descendant.clone();
                    let active = active.clone();
                    let peak = peak.clone();
                    move |id| {
                        let descendant = descendant.clone();
                        let active = active.clone();
                        let peak = peak.clone();
                        async move {
                            let n = active.fetch_add(1, Ordering::SeqCst) + 1;
                            peak.fetch_max(n, Ordering::SeqCst);
                            let _active = Active(active);
                            let rows = if id == "root" {
                                (0..concurrency)
                                    .map(|n| json!({"id":format!("s-{n}"),"isFolder":true}))
                                    .collect()
                            } else if id == "s-0" {
                                vec![json!({"id":"descendant","isFolder":true})]
                            } else if id == "descendant" {
                                descendant.add_permits(concurrency - 1);
                                vec![json!({"id":"file"})]
                            } else {
                                descendant.acquire().await.unwrap().forget();
                                vec![]
                            };
                            Ok::<_, ()>(rows)
                        }
                    }
                },
                "root",
                "",
                concurrency,
            ),
        )
        .await
        .expect("batch scheduling would deadlock on the slow siblings")
        .unwrap();
        assert_eq!(tree.visited, concurrency + 2);
        assert_eq!(tree.all_files.len(), 1);
        assert!(peak.load(Ordering::SeqCst) <= concurrency);
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn cancellation_drops_every_running_callback() {
    use futures_util::poll;
    let active = Arc::new(AtomicUsize::new(0));
    let mut pending = Box::pin(walk_complete(
        {
            let active = active.clone();
            move |id| {
                let active = active.clone();
                async move {
                    if id == "root" {
                        return Ok::<_, ()>(
                            (0..4)
                                .map(|n| json!({"id":n.to_string(),"isFolder":true}))
                                .collect(),
                        );
                    }
                    active.fetch_add(1, Ordering::SeqCst);
                    let _active = Active(active);
                    std::future::pending::<()>().await;
                    Ok(vec![])
                }
            }
        },
        "root",
        "",
        4,
    ));
    assert!(poll!(pending.as_mut()).is_pending());
    assert_eq!(active.load(Ordering::SeqCst), 4);
    drop(pending);
    assert_eq!(active.load(Ordering::SeqCst), 0);
}
