//! Executes the pinned original class + shared walker, then compares complete JSON.
//! The module is path-included until the parent registers it in providers/mod.rs.
mod support;
pub use jxpan_rust::{compat, providers, tree};
#[path = "../src/providers/mcloud_service.rs"]
mod mcloud_service;
use mcloud_service::{ServiceConfig, fid_chain_with_callbacks, parse_with_callbacks};
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    future::poll_fn,
    rc::Rc,
    task::Poll,
};

#[derive(Default)]
struct Trace {
    lists: RefCell<Vec<String>>,
    downloads: RefCell<Vec<String>>,
    active: Cell<usize>,
    peak: Cell<usize>,
}
fn oracle() -> Value {
    support::oracle(
        "mcloud-service",
        include_str!("fixtures/mcloud-service.json"),
    )
}
async fn execute(input: &Value) -> (Value, Rc<Trace>) {
    let trace = Rc::new(Trace::default());
    let list_trace = trace.clone();
    let graph = input["graph"].clone();
    let failures = input["failures"].as_array().cloned().unwrap_or_default();
    let list = move |id: String| {
        let trace = list_trace.clone();
        trace.lists.borrow_mut().push(id.clone());
        let mut entries = Some(
            graph
                .get(&id)
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        );
        let fails = failures.iter().any(|v| v.as_str() == Some(&id));
        let mut turns = 1 + id.chars().map(|c| c as usize).sum::<usize>() % 4;
        let mut started = false;
        poll_fn(move |cx| {
            if !started {
                started = true;
                trace.active.set(trace.active.get() + 1);
                trace.peak.set(trace.peak.get().max(trace.active.get()));
            }
            if turns > 0 {
                turns -= 1;
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
            trace.active.set(trace.active.get() - 1);
            Poll::Ready(if fails {
                Err("private error must never appear in JSON")
            } else {
                Ok(entries.take().expect("completion only once"))
            })
        })
    };
    let download_trace = trace.clone();
    let download_fails = input["downloadFailure"].as_bool().unwrap_or(false);
    let download_null = input["downloadNull"].as_bool().unwrap_or(false);
    let download = move |path: String| {
        download_trace.downloads.borrow_mut().push(path.clone());
        async move {
            if download_fails {
                Err("private download failure")
            } else if download_null {
                Ok(None)
            } else {
                // encodeURIComponent for the synthetic fixtures: slash is percent-escaped.
                let encoded: String = path
                    .bytes()
                    .map(|b| {
                        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
                            (b as char).to_string()
                        } else {
                            format!("%{b:02X}")
                        }
                    })
                    .collect();
                Ok(Some(format!("https://download.invalid/offline/{encoded}")))
            }
        }
    };
    let chain: Vec<String> = input["chain"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    let target = input["target"].as_str().unwrap();
    let result = if input["action"] == "fidChain" {
        fid_chain_with_callbacks("Offline123", &chain, target, list, download).await
    } else {
        parse_with_callbacks(
            ServiceConfig {
                enabled: input["enabled"].as_bool().unwrap(),
                authorization_configured: input["authorizationConfigured"].as_bool().unwrap(),
            },
            input["shareUrl"].as_str().unwrap(),
            &chain,
            target,
            list,
            download,
        )
        .await
    };
    (result, trace)
}

#[tokio::test(flavor = "current_thread")]
async fn exact_original_orchestration_differential() {
    let vectors = oracle();
    assert_eq!(vectors["sourceHash"], providers::mcloud::DEPLOYMENT_SHA256);
    let mut saved = 0;
    for vector in vectors["vectors"].as_array().unwrap() {
        let input = &vector["input"];
        let name = input["name"].as_str().unwrap();
        let (result, trace) = execute(input).await;
        assert_eq!(
            result, vector["expected"]["output"],
            "complete JSON: {name}"
        );
        assert_eq!(
            json!(*trace.lists.borrow()),
            vector["optimizedCalls"],
            "list/DFS ordering: {name}"
        );
        assert_eq!(
            json!(*trace.downloads.borrow()),
            vector["expected"]["downloadCalls"],
            "download/path priority: {name}"
        );
        assert_eq!(trace.active.get(), 0, "all callbacks awaited: {name}");
        assert!(trace.peak.get() <= 4, "four concurrent callbacks: {name}");
        let original = vector["expected"]["calls"].as_array().unwrap().len();
        let actual = trace.lists.borrow().len();
        saved += original - actual;
        if name == "tree-four-concurrency-400-nodes" {
            assert_eq!(actual, 400);
            assert_eq!(trace.peak.get(), 4);
        }
    }
    assert_eq!(saved as u64, vectors["savedRootRequests"].as_u64().unwrap());
    println!(
        "{} exact original JSON vectors; {saved} redundant root requests eliminated",
        vectors["vectors"].as_array().unwrap().len()
    );
}

#[test]
fn production_adapter_signatures_are_type_checked() {
    // No real network: references keep the production adapters compiled in this integration target.
    let _ = mcloud_service::parse;
    let _ = mcloud_service::parse_with_fid_chain;
}
