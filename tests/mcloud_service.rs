//! Offline regression tests for the checked mobile service.
//! The frozen original corpus is evidence of legacy behavior, not an exact-JSON
//! or callback-order contract for production. Corrected behavior also has
//! independent synthetic expectations below; the original fixture is untouched.
pub use jxpan_rust::{compat, providers, settings, tree};
#[path = "../src/providers/mcloud_service.rs"]
mod mcloud_service;
use mcloud_service::{ServiceConfig, fid_chain_with_callbacks, parse_indexed_with_callbacks};
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeSet,
    future::{Future, poll_fn},
    rc::Rc,
    task::Poll,
    time::Duration,
};

const SHARE_URL: &str = "https://yun.139.com/shareweb/#/w/i/Offline123";
const CONFIG: ServiceConfig = ServiceConfig {
    enabled: true,
    authorization_configured: true,
};
const PRIVATE_LIST_ERROR: &str = "private list error: synthetic sentinel";
const PRIVATE_DOWNLOAD_ERROR: &str = "private download error: synthetic sentinel";
type Index = Option<(String, Vec<String>)>;

#[derive(Default)]
struct Trace {
    lists: RefCell<Vec<String>>,
    downloads: RefCell<Vec<String>>,
    started: RefCell<Vec<String>>,
    completed: RefCell<Vec<String>>,
    cancelled: RefCell<Vec<String>>,
    events: RefCell<Vec<String>>,
    active: Cell<usize>,
    peak: Cell<usize>,
}

// A callback future can be dropped before it is first polled or while pending.
// Account for both: early errors legitimately cancel siblings, not leak them.
struct ListLease {
    trace: Rc<Trace>,
    id: String,
    started: bool,
    finished: bool,
}
impl ListLease {
    fn new(trace: Rc<Trace>, id: String) -> Self {
        trace.lists.borrow_mut().push(id.clone());
        trace.events.borrow_mut().push(format!("list:{id}"));
        Self {
            trace,
            id,
            started: false,
            finished: false,
        }
    }
    fn start(&mut self) {
        if !self.started {
            self.started = true;
            self.trace.started.borrow_mut().push(self.id.clone());
            self.trace.active.set(self.trace.active.get() + 1);
            self.trace
                .peak
                .set(self.trace.peak.get().max(self.trace.active.get()));
        }
    }
    fn finish(&mut self) {
        assert!(self.started && !self.finished);
        self.finished = true;
        self.trace.active.set(self.trace.active.get() - 1);
        self.trace.completed.borrow_mut().push(self.id.clone());
        self.trace
            .events
            .borrow_mut()
            .push(format!("complete:{}", self.id));
    }
}
impl Drop for ListLease {
    fn drop(&mut self) {
        if !self.finished {
            if self.started {
                self.trace.active.set(self.trace.active.get() - 1);
            }
            self.trace.cancelled.borrow_mut().push(self.id.clone());
            self.trace
                .events
                .borrow_mut()
                .push(format!("cancel:{}", self.id));
        }
    }
}

fn assert_quiescent(trace: &Trace) {
    assert_eq!(
        trace.active.get(),
        0,
        "no pending callback survives the request"
    );
    assert!(
        trace.peak.get() <= settings::DEFAULT_CONCURRENCY,
        "callbacks must stay within the configured default bound"
    );
    let mut admitted = trace.lists.borrow().clone();
    let mut settled = trace.completed.borrow().clone();
    settled.extend(trace.cancelled.borrow().iter().cloned());
    admitted.sort();
    settled.sort();
    assert_eq!(
        admitted, settled,
        "every admitted future completed or was dropped"
    );
}
fn assert_lists(trace: &Trace, expected: &[&str]) {
    // Check request scope and multiplicity, deliberately not scheduling order.
    let mut actual = trace.lists.borrow().clone();
    actual.sort();
    let mut expected: Vec<_> = expected.iter().map(|id| (*id).to_owned()).collect();
    expected.sort();
    assert_eq!(actual, expected);
}
fn assert_index(index: &Index, expected: &[&str]) {
    let (link, ids) = index.as_ref().expect("complete directory index");
    assert_eq!(link, "Offline123");
    let actual: BTreeSet<_> = ids.iter().map(String::as_str).collect();
    assert_eq!(actual.len(), ids.len(), "index IDs must be unique");
    assert_eq!(actual, expected.iter().copied().collect());
}
fn assert_success(value: &Value) {
    assert_eq!(value["code"], 200);
    assert_eq!(value["success"], true);
    assert_eq!(value["shareKey"], "mc:Offline123");
    assert!(!value["data"].is_null());
}
fn assert_error(value: &Value, code: u16) {
    assert_eq!(value["code"], code);
    assert_eq!(value["success"], false);
    assert_eq!(
        value["data"],
        Value::Null,
        "never return a successful partial tree"
    );
    assert!(value["msg"].as_str().is_some_and(|s| !s.is_empty()));
    assert!(value.get("shareKey").is_none());
    let serialized = value.to_string();
    assert!(
        !serialized.contains("private"),
        "supplier errors must be redacted"
    );
    assert!(!serialized.contains(PRIVATE_LIST_ERROR));
    assert!(!serialized.contains(PRIVATE_DOWNLOAD_ERROR));
}
fn input(graph: Value) -> Value {
    json!({"graph":graph,"action":"parse","enabled":true,
        "authorizationConfigured":true,"shareUrl":SHARE_URL,"chain":[],"target":""})
}
fn file(id: &str, name: &str, size: Value) -> Value {
    json!({"coID":id,"coName":name,"coSize":size})
}
fn folder(id: &str, name: &str) -> Value {
    json!({"coID":id,"coName":name,"coType":1})
}
fn oracle() -> Value {
    // Do not use support::oracle: an inherited JXPAN_LIVE_ORACLE must not opt
    // this target into reading a private deployment or invoking an external oracle.
    serde_json::from_str(include_str!("fixtures/mcloud-service.json")).unwrap()
}
fn vector<'a>(vectors: &'a Value, name: &str) -> &'a Value {
    vectors["vectors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["input"]["name"] == name)
        .unwrap_or_else(|| panic!("missing frozen vector: {name}"))
}

async fn execute(input: &Value) -> (Value, Index, Rc<Trace>) {
    let trace = Rc::new(Trace::default());
    let list_trace = trace.clone();
    let graph = input["graph"].clone();
    let failures = input["failures"].as_array().cloned().unwrap_or_default();
    let list = move |id: String| {
        let mut lease = ListLease::new(list_trace.clone(), id.clone());
        let mut entries = Some(
            graph
                .get(&id)
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        );
        let fails = failures.iter().any(|v| v.as_str() == Some(&id));
        let mut turns = 1 + id.chars().map(|c| c as usize).sum::<usize>() % 4;
        poll_fn(move |cx| {
            lease.start();
            if turns > 0 {
                turns -= 1;
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
            lease.finish();
            Poll::Ready(if fails {
                Err(PRIVATE_LIST_ERROR)
            } else {
                Ok(entries.take().expect("completion only once"))
            })
        })
    };
    let download_trace = trace.clone();
    let download_fails = input["downloadFailure"].as_bool().unwrap_or(false);
    let download_null = input["downloadNull"].as_bool().unwrap_or(false);
    let download_empty = input["downloadEmpty"].as_bool().unwrap_or(false);
    let download = move |id: String| {
        download_trace.downloads.borrow_mut().push(id.clone());
        async move {
            if download_fails {
                Err(PRIVATE_DOWNLOAD_ERROR)
            } else if download_null {
                Ok(None)
            } else if download_empty {
                Ok(Some(String::new()))
            } else {
                let encoded: String = id
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
    let (result, index) = if input["action"] == "fidChain" {
        (
            fid_chain_with_callbacks("Offline123", &chain, target, list, download).await,
            None,
        )
    } else {
        parse_indexed_with_callbacks(
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
    assert_quiescent(&trace);
    (result, index, trace)
}

// Compare meaningful old fields while allowing new metadata, not an entire
// response snapshot. Messages and callback scheduling are not legacy contracts.
fn assert_legacy_fields(actual: &Value, expected: &Value, context: &str) {
    match expected {
        Value::Object(fields) => {
            assert!(
                actual.is_object(),
                "{context}: expected object, got {actual}"
            );
            for (key, value) in fields {
                assert!(actual.get(key).is_some(), "{context}: missing {key}");
                assert_legacy_fields(&actual[key], value, &format!("{context}/{key}"));
            }
        }
        Value::Array(rows) => {
            let actual = actual.as_array().expect("legacy array shape");
            assert_eq!(actual.len(), rows.len(), "{context}: row count");
            for (i, (actual, expected)) in actual.iter().zip(rows).enumerate() {
                assert_legacy_fields(actual, expected, &format!("{context}/{i}"));
            }
        }
        _ => assert_eq!(actual, expected, "{context}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn frozen_original_normal_cases_preserve_meaningful_fields() {
    let vectors = oracle();
    assert_eq!(vectors["sourceHash"], providers::mcloud::DEPLOYMENT_SHA256);
    let names = [
        "disabled-before-auth-and-link",
        "authorization-missing-before-link",
        "invalid-link",
        "multi-file-tree",
        "multilevel-empty-duplicate-names",
        "fid-last-item-only",
        "fileID-is-folder",
        "fileID-missing",
        "filesize-0",
        "filesize-1",
        "filesize-1023",
        "filesize-1024",
        "filesize-1152",
        "filesize-1280",
        "filesize-1408",
        "filesize-1536",
        "filesize-1048576",
        "filesize-\"1099511627776\"",
        "chain-full-list",
        "chain-single-level-parent-root",
        "chain-empty-means-root-list",
    ];
    for name in names {
        let vector = vector(&vectors, name);
        let expected = &vector["expected"]["output"];
        let (actual, _, trace) = execute(&vector["input"]).await;
        assert_eq!(actual["code"], expected["code"], "status: {name}");
        assert_eq!(actual["success"], expected["success"], "success: {name}");
        assert_legacy_fields(&actual["data"], &expected["data"], name);
        if expected["success"] == true {
            assert_eq!(
                actual["shareKey"], expected["shareKey"],
                "share key: {name}"
            );
        }
        // At most one read per scoped directory; the original repeated root and
        // batch/DFS call order intentionally no longer determine correctness.
        let requests = trace.lists.borrow();
        assert_eq!(
            requests.len(),
            requests.iter().collect::<BTreeSet<_>>().len()
        );
        assert!(trace.downloads.borrow().is_empty(), "no prefetch: {name}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn frozen_original_correctness_differences_are_explicit() {
    let vectors = oracle();
    // These old successful/empty results are bugs, not outputs to imitate.
    for (name, old_code, new_code) in [
        ("empty-share", 404, 200),
        ("failed-authentication-is-empty", 404, 502),
        ("single-file-never-download", 200, 502), // malformed "1024bytes"
        ("single-file-missing-fields", 200, 502),
        ("fid-missing-keeps-requested-tree", 200, 404),
        ("fileID-download-null", 200, 502),
        ("fileID-download-error", 200, 502),
        ("fileID-visited-cycle", 404, 502),
        ("tree-child-failure-is-empty-node", 200, 502),
        ("fileID-child-failure-continues", 200, 502),
        ("fileID-depth-nine-excluded", 404, 200),
        ("chain-second-folder-empty", 404, 200),
    ] {
        let vector = vector(&vectors, name);
        assert_eq!(
            vector["expected"]["output"]["code"], old_code,
            "legacy: {name}"
        );
        let (actual, _, _) = execute(&vector["input"]).await;
        if new_code == 200 {
            assert_success(&actual);
        } else {
            assert_error(&actual, new_code);
        }
    }

    let priority = vector(&vectors, "single-file-field-priority");
    assert_eq!(
        priority["expected"]["output"]["data"]["list"][0]["file_id"],
        "co"
    );
    let (actual, _, trace) = execute(&priority["input"]).await;
    assert_eq!(actual["data"]["list"][0]["file_id"], "file");
    assert_eq!(actual["data"]["list"][0]["file_name"], "fileName");
    assert_eq!(actual["data"]["list"][0]["file_size_bytes"], 4);
    assert!(trace.downloads.borrow().is_empty());

    for (name, id) in [
        ("fileID-DFS-path", "movie"),
        ("fileID-wins-over-fid", "movie"),
        ("fileID-path-before-coPath", "f"),
        ("fileID-coPath", "f"),
        ("chain-file", "f"),
    ] {
        let vector = vector(&vectors, name);
        assert_ne!(
            vector["expected"]["downloadCalls"],
            json!([id]),
            "legacy path: {name}"
        );
        let (actual, _, trace) = execute(&vector["input"]).await;
        assert_success(&actual);
        assert_eq!(*trace.downloads.borrow(), [id], "ID, not path: {name}");
        assert_eq!(actual["data"]["file_id"], id);
        assert_eq!(
            actual["data"]["download_url"],
            format!("https://download.invalid/offline/{id}")
        );
    }

    let wide = vector(&vectors, "tree-four-concurrency-400-nodes");
    let (actual, _, trace) = execute(&wide["input"]).await;
    assert_success(&actual);
    // The old projection listed all names but fetched only 400 directories;
    // empty placeholders did not prove that the tree was complete.
    assert_eq!(wide["optimizedCalls"].as_array().unwrap().len(), 400);
    assert_eq!(
        actual["data"]["tree"]["移动云盘分享"]
            .as_object()
            .unwrap()
            .len(),
        420
    );
    assert_eq!(actual["data"]["folder_count"], 421);
    assert_eq!(trace.lists.borrow().len(), 421);

    let classifier = vector(&vectors, "shared-walker-classifier-not-parser");
    let (actual, _, _) = execute(&classifier["input"]).await;
    let old_tree = &classifier["expected"]["output"]["data"]["tree"]["移动云盘分享"];
    for name in ["resType-folder", "type-folder", "coType-string"] {
        assert!(old_tree[name].get("id").is_some(), "legacy file classifier");
        assert_eq!(actual["data"]["tree"]["移动云盘分享"][name], json!({}));
    }

    let chain = vector(&vectors, "chain-first-folder-failure");
    assert_ne!(chain["expected"]["output"]["code"], 200);
    let (actual, _, trace) = execute(&chain["input"]).await;
    assert_success(&actual);
    assert_lists(&trace, &["deep"]);
    assert_eq!(actual["data"]["current_folder_id"], "deep");
}

#[tokio::test(flavor = "current_thread")]
async fn gates_do_not_start_io() {
    for (enabled, authorization, url, code) in [
        (false, false, "invalid", 503),
        (true, false, "invalid", 401),
        (true, true, "invalid", 400),
    ] {
        let mut request = input(json!({}));
        request["enabled"] = json!(enabled);
        request["authorizationConfigured"] = json!(authorization);
        request["shareUrl"] = json!(url);
        let (value, index, trace) = execute(&request).await;
        assert_error(&value, code);
        assert_eq!(index, None);
        assert_lists(&trace, &[]);
        assert!(trace.downloads.borrow().is_empty());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn empty_root_succeeds_but_missing_fid_or_file_is_404() {
    let (value, index, trace) = execute(&input(json!({"root":[]}))).await;
    assert_success(&value);
    assert_eq!(
        value["data"],
        json!({"tree":{"移动云盘分享":{}},"sid":"Offline123",
        "tree_complete":true,"file_count":0,"folder_count":1})
    );
    assert_index(&index, &["root"]);
    assert_lists(&trace, &["root"]);
    assert!(trace.downloads.borrow().is_empty());

    let root_reads = Cell::new(0);
    let public = mcloud_service::parse_with_callbacks(
        CONFIG,
        SHARE_URL,
        &[],
        "",
        |id| {
            assert_eq!(id, "root");
            root_reads.set(root_reads.get() + 1);
            async { Ok::<_, &'static str>(vec![]) }
        },
        |_| async {
            panic!("the public parse callback API must not prefetch a download");
            #[allow(unreachable_code)]
            Ok::<_, &'static str>(None)
        },
    )
    .await;
    assert_eq!(
        public, value,
        "public parse delegates the same corrected semantics"
    );
    assert_eq!(root_reads.get(), 1);

    let mut request = input(json!({"root":[folder("empty", "空目录")],"empty":[]}));
    for (chain, target) in [(json!(["missing"]), ""), (json!([]), "missing-file")] {
        request["chain"] = chain;
        request["target"] = json!(target);
        let (value, index, trace) = execute(&request).await;
        assert_error(&value, 404);
        assert_index(&index, &["root", "empty"]);
        assert_lists(&trace, &["root", "empty"]);
        assert!(trace.downloads.borrow().is_empty());
    }
    request["chain"] = json!(["ignored-ancestor", "empty"]);
    request["target"] = json!("");
    let (value, _, trace) = execute(&request).await;
    assert_success(&value);
    assert_eq!(value["data"]["tree"], json!({"空目录":{}}));
    assert_lists(&trace, &["root", "empty"]);
}

#[tokio::test(flavor = "current_thread")]
async fn listing_failure_is_502_without_partial_data_or_index() {
    let graph = json!({"root":[folder("bad", "失败"),folder("good", "正常")],
        "good":[file("f", "文件", json!(1))]});
    for failed in ["root", "bad"] {
        for target in ["", "f"] {
            let mut request = input(graph.clone());
            request["failures"] = json!([failed]);
            request["target"] = json!(target);
            let (value, index, trace) = execute(&request).await;
            assert_error(&value, 502);
            assert_eq!(index, None);
            assert!(
                trace.downloads.borrow().is_empty(),
                "do not download after incomplete discovery"
            );
            if failed == "root" {
                assert_lists(&trace, &["root"]);
            } else {
                assert!(trace.lists.borrow().iter().any(|id| id == "bad"));
            }
        }
    }
    let mut request = input(graph);
    request["action"] = json!("fidChain");
    request["chain"] = json!(["ignored", "bad"]);
    request["failures"] = json!(["bad"]);
    let (value, _, trace) = execute(&request).await;
    assert_error(&value, 502);
    assert_lists(&trace, &["bad"]);
    assert!(trace.downloads.borrow().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn depth_twelve_is_complete_for_tree_and_targeted_download() {
    let mut graph = json!({"root":[folder("d1", "D1")]});
    for depth in 1..=12 {
        graph[format!("d{depth}")] = if depth < 12 {
            json!([folder(
                &format!("d{}", depth + 1),
                &format!("D{}", depth + 1)
            )])
        } else {
            json!([file("deep-file", "终点", json!(1536))])
        };
    }
    let mut request = input(graph);
    let (value, index, trace) = execute(&request).await;
    assert_success(&value);
    assert_eq!(value["data"]["tree_complete"], true);
    assert_eq!(value["data"]["file_count"], 1);
    assert_eq!(value["data"]["folder_count"], 13);
    let mut leaf = &value["data"]["tree"]["移动云盘分享"];
    for depth in 1..=12 {
        leaf = &leaf[format!("D{depth}")];
    }
    assert_eq!(leaf, &json!({"终点":{"id":"deep-file","size":1536}}));
    let ids: Vec<_> = std::iter::once("root".to_owned())
        .chain((1..=12).map(|i| format!("d{i}")))
        .collect();
    let ids: Vec<_> = ids.iter().map(String::as_str).collect();
    assert_lists(&trace, &ids);
    assert_index(&index, &ids);
    assert!(trace.downloads.borrow().is_empty());

    request["target"] = json!("deep-file");
    let (value, index, trace) = execute(&request).await;
    assert_success(&value);
    assert_eq!(value["data"]["file_id"], "deep-file");
    assert_eq!(value["data"]["file_size_bytes"], 1536);
    assert_eq!(*trace.downloads.borrow(), ["deep-file"]);
    assert_lists(&trace, &ids);
    assert_index(&index, &ids);
}

#[tokio::test(flavor = "current_thread")]
async fn four_hundred_fifty_directories_are_not_silently_truncated() {
    let mut graph = json!({"root":[]});
    for i in 0..450 {
        let id = format!("d{i}");
        graph["root"].as_array_mut().unwrap().push(folder(&id, &id));
        graph[&id] = json!([file(&format!("f{i}"), "叶子", json!(i))]);
    }
    let (value, index, trace) = execute(&input(graph)).await;
    assert_success(&value);
    assert_eq!(value["data"]["tree_complete"], true);
    assert_eq!(value["data"]["folder_count"], 451);
    assert_eq!(value["data"]["file_count"], 450);
    let body = value["data"]["tree"]["移动云盘分享"].as_object().unwrap();
    assert_eq!(body.len(), 450);
    for i in 0..450 {
        assert_eq!(
            body[&format!("d{i}")],
            json!({"叶子":{"id":format!("f{i}"),"size":i}})
        );
    }
    let ids: Vec<_> = std::iter::once("root".to_owned())
        .chain((0..450).map(|i| format!("d{i}")))
        .collect();
    let ids: Vec<_> = ids.iter().map(String::as_str).collect();
    assert_lists(&trace, &ids);
    assert_index(&index, &ids);
    assert_eq!(trace.peak.get(), settings::DEFAULT_CONCURRENCY);
    assert!(trace.downloads.borrow().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn safety_limit_and_cycle_are_errors_not_empty_trees() {
    let mut graph = json!({"root":[folder("d1", "d1")]});
    for i in 1..=tree::MAX_PRODUCTION_DEPTH {
        graph[format!("d{i}")] = json!([folder(&format!("d{}", i + 1), "D")]);
    }
    let (value, index, trace) = execute(&input(graph)).await;
    assert_error(&value, 413);
    assert_eq!(index, None);
    assert!(trace.downloads.borrow().is_empty());

    let graph = json!({"root":[folder("a", "A")],"a":[folder("root", "循环")]});
    let (value, index, trace) = execute(&input(graph)).await;
    assert_error(&value, 502);
    assert_eq!(index, None);
    assert_lists(&trace, &["root", "a"]);
    assert!(trace.downloads.borrow().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn byte_sizes_preserve_zero_large_integers_and_unknown() {
    let cases = [
        (
            json!({"fileId":"zero","fileName":"zero","fileSize":0,"size":999,"coSize":123}),
            json!(0),
            Some("0.00 B"),
        ),
        (
            json!({"coID":"string-zero","coSize":"0"}),
            json!("0"),
            Some("0.00 B"),
        ),
        (
            json!({"coID":"safe","coSize":9_007_199_254_740_991_u64}),
            json!(9_007_199_254_740_991_u64),
            None,
        ),
        (
            json!({"coID":"large","coSize":9_007_199_254_740_993_u64}),
            json!("9007199254740993"),
            None,
        ),
        (
            json!({"coID":"max","coSize":u64::MAX}),
            json!("18446744073709551615"),
            None,
        ),
        (
            json!({"coID":"decimal","coSize":" 9007199254740993 "}),
            json!("9007199254740993"),
            None,
        ),
        (json!({"coID":"unknown"}), Value::Null, Some("未知")),
        (
            json!({"coID":"null-size","fileSize":null}),
            Value::Null,
            Some("未知"),
        ),
    ];
    for (entry, bytes, display) in &cases {
        let (value, index, trace) = execute(&input(json!({"root":[entry]}))).await;
        assert_success(&value);
        assert_eq!(index, None, "single-file homepage is not a walk index");
        let row = &value["data"]["list"][0];
        assert_eq!(&row["file_size_bytes"], bytes, "exact byte count: {entry}");
        assert_eq!(row["is_folder"], false);
        assert_eq!(row["download_url"], Value::Null);
        if let Some(display) = display {
            assert_eq!(row["file_size"], *display);
        }
        assert_lists(&trace, &["root"]);
        assert!(trace.downloads.borrow().is_empty());
    }
    let entries: Vec<_> = cases.iter().map(|(entry, _, _)| entry.clone()).collect();
    let mut request = input(json!({"root":entries}));
    let (value, _, trace) = execute(&request).await;
    assert_success(&value);
    for (entry, bytes, _) in &cases {
        let id = entry["fileId"]
            .as_str()
            .or_else(|| entry["coID"].as_str())
            .unwrap();
        assert_eq!(&value["data"]["tree"]["移动云盘分享"][id]["size"], bytes);
    }
    assert!(trace.downloads.borrow().is_empty());
    request["action"] = json!("fidChain");
    let (value, _, trace) = execute(&request).await;
    assert_success(&value);
    for (row, (_, bytes, _)) in value["data"]["list"].as_array().unwrap().iter().zip(&cases) {
        assert_eq!(&row["file_size_bytes"], bytes);
    }
    assert_lists(&trace, &["root"]);
    assert!(trace.downloads.borrow().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn null_or_missing_id_and_invalid_sizes_fail_instead_of_fabricating_files() {
    for bad in [
        json!({}),
        json!({"coID":null,"coName":"no ID"}),
        json!({"fileId":null,"id":null,"coID":null,"contentID":null}),
        Value::Null,
    ] {
        // Exercise the single-file fast path, the checked walker, and direct fid listing.
        for mode in ["single", "child", "fid"] {
            let mut request = if mode == "child" {
                input(json!({"root":[folder("a", "A")],"a":[bad]}))
            } else {
                input(json!({"root":[bad]}))
            };
            if mode == "fid" {
                request["action"] = json!("fidChain");
            }
            let (value, index, trace) = execute(&request).await;
            assert_error(&value, 502);
            assert_eq!(index, None);
            assert!(trace.downloads.borrow().is_empty());
        }
    }
    for size in [
        json!(-1),
        json!(1.5),
        json!("1024bytes"),
        json!("NaN"),
        json!(true),
    ] {
        let (value, index, trace) = execute(&input(json!({"root":[file("f", "F", size)]}))).await;
        assert_error(&value, 502);
        assert_eq!(index, None);
        assert!(trace.downloads.borrow().is_empty());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn explicit_folder_flags_override_generic_type_and_index_matches_actual_walk() {
    let graph = json!({"root":[
        {"coID":"file-false","coName":"false","isFolder":false,"isDir":true,"coType":1,"coSize":0},
        {"coID":"file-string","coName":"string false","isFolder":"false","type":"folder"},
        {"coID":"file-zero","coName":"zero flag","isDir":"0","resType":1},
        {"coID":"null-flag","coName":"null flag","isFolder":null,"isDir":"true","coType":0},
        {"coID":"string-type","coName":"string type","coType":"1"},
        {"coID":"generic-type","coName":"generic type","type":"folder"},
        {"coID":"resource-type","coName":"resource type","resType":1},
        {"coID":"directory-flag","coName":"directory flag","isDirectory":true},
        {"coID":"true-first","coName":"true first","isFolder":true,"isDir":false}
    ]});
    let (value, index, trace) = execute(&input(graph)).await;
    assert_success(&value);
    assert_eq!(
        value["data"]["tree"],
        json!({"移动云盘分享":{
            "null flag":{},"string type":{},"generic type":{},"resource type":{},
            "directory flag":{},"true first":{},
            "false":{"id":"file-false","size":0},
            "string false":{"id":"file-string","size":null},
            "zero flag":{"id":"file-zero","size":null}
        }})
    );
    let directories = [
        "root",
        "null-flag",
        "string-type",
        "generic-type",
        "resource-type",
        "directory-flag",
        "true-first",
    ];
    assert_index(&index, &directories);
    assert_lists(&trace, &directories);
    assert_eq!(value["data"]["folder_count"], 7);
    assert_eq!(value["data"]["file_count"], 3);
    assert!(trace.downloads.borrow().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn direct_fid_lists_only_last_directory_once_and_never_prefetches() {
    let mut request = input(json!({"last":[folder("child", "子目录"),
        file("f", "文件", json!(0)),{"contentID":"u","contentName":"未知"}]}));
    request["action"] = json!("fidChain");
    request["chain"] = json!(["nonexistent-ancestor", "parent", "last"]);
    let (value, index, trace) = execute(&request).await;
    assert_success(&value);
    assert_eq!(index, None);
    assert_eq!(
        value["data"],
        json!({"name":"移动云盘子文件夹","desc":"",
        "folder":[{"id":"child","name":"子目录"}],
        "list":[
            {"file_id":"f","file_name":"文件","file_size":"0.00 B","file_size_bytes":0,"is_folder":false,"download_url":null},
            {"file_id":"u","file_name":"未知","file_size":"未知","file_size_bytes":null,"is_folder":false,"download_url":null}
        ],"have_page":false,"is_mcloud_folder":true,"share_id":"Offline123",
        "current_folder_id":"last","parent_folder_id":"parent"})
    );
    assert_lists(&trace, &["last"]);
    assert!(trace.downloads.borrow().is_empty());
    for (target, code) in [("missing", 404), ("child", 400)] {
        request["target"] = json!(target);
        let (value, _, trace) = execute(&request).await;
        assert_error(&value, code);
        assert_lists(&trace, &["last"]);
        assert!(trace.downloads.borrow().is_empty());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn targeted_download_uses_file_id_not_supplier_or_traversal_path() {
    let graph = json!({"root":[folder("a", "目录")],"a":[
        {"coID":"file-id","coName":"文件.mkv","coSize":9007199254740993_u64,
            "path":"/supplier/path","coPath":"/other/path"}
    ]});
    let mut request = input(graph.clone());
    let (value, _, trace) = execute(&request).await;
    assert_success(&value);
    assert_lists(&trace, &["root", "a"]);
    assert!(trace.downloads.borrow().is_empty());

    request["target"] = json!("file-id");
    request["chain"] = json!(["missing-fid"]); // fileID takes precedence.
    let (value, index, trace) = execute(&request).await;
    assert_success(&value);
    assert_eq!(
        value["data"],
        json!({"file_id":"file-id","file_name":"文件.mkv",
        "file_size":"8192.00 TB","file_size_bytes":"9007199254740993","is_folder":false,
        "download_url":"https://download.invalid/offline/file-id"})
    );
    assert_index(&index, &["root", "a"]);
    assert_lists(&trace, &["root", "a"]);
    assert_eq!(*trace.downloads.borrow(), ["file-id"]);

    request["action"] = json!("fidChain");
    request["chain"] = json!(["ignored", "a"]);
    let (direct, _, trace) = execute(&request).await;
    assert_eq!(direct["data"], value["data"]);
    assert_lists(&trace, &["a"]);
    assert_eq!(*trace.downloads.borrow(), ["file-id"]);
}

#[tokio::test(flavor = "current_thread")]
async fn download_error_null_or_empty_url_is_never_success_or_private_error_echo() {
    for action in ["parse", "fidChain"] {
        for failure in ["downloadFailure", "downloadNull", "downloadEmpty"] {
            let mut request = input(json!({"root":[file("f", "F", json!(1))]}));
            request["action"] = json!(action);
            request["target"] = json!("f");
            request[failure] = json!(true);
            let (value, index, trace) = execute(&request).await;
            assert_error(&value, 502);
            if action == "parse" {
                assert_index(&index, &["root"]);
            } else {
                assert_eq!(index, None);
            }
            assert_lists(&trace, &["root"]);
            assert_eq!(*trace.downloads.borrow(), ["f"]);
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn rolling_walker_admits_fast_child_before_slow_siblings_can_finish() {
    let trace = Rc::new(Trace::default());
    let list_trace = trace.clone();
    let child_admitted = Rc::new(Cell::new(false));
    let admission = child_admitted.clone();
    let list = move |id: String| {
        let mut lease = ListLease::new(list_trace.clone(), id.clone());
        let admission = admission.clone();
        if id == "fast-child" {
            admission.set(true);
        }
        poll_fn(move |cx| {
            lease.start();
            let ready = match id.as_str() {
                "fast" => ["slow", "left", "right"].iter().all(|sibling| {
                    lease
                        .trace
                        .started
                        .borrow()
                        .iter()
                        .any(|started| started == sibling)
                }),
                "slow" | "left" | "right" => admission.get(),
                _ => true,
            };
            if !ready {
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
            let entries = match id.as_str() {
                "root" => vec![
                    folder("fast", "Fast"),
                    folder("slow", "Slow"),
                    folder("left", "Left"),
                    folder("right", "Right"),
                ],
                "fast" => vec![folder("fast-child", "Child")],
                "fast-child" => vec![file("f", "F", json!(1))],
                _ => vec![],
            };
            lease.finish();
            Poll::Ready(Ok::<_, &'static str>(entries))
        })
    };
    // A deadlock watchdog only, not a latency/performance assertion: a batch
    // walker cannot finish because siblings require admission of the fast child.
    let (value, index) = tokio::time::timeout(
        Duration::from_secs(5),
        parse_indexed_with_callbacks(CONFIG, SHARE_URL, &[], "", list, |_| async {
            panic!("tree traversal must not prefetch a download");
            #[allow(unreachable_code)]
            Ok::<_, &'static str>(None)
        }),
    )
    .await
    .expect("batch scheduling deadlocked: fast child was not admitted");
    assert_success(&value);
    assert_eq!(
        value["data"]["tree"]["移动云盘分享"]["Fast"]["Child"],
        json!({"F":{"id":"f","size":1}})
    );
    assert_index(
        &index,
        &["root", "fast", "slow", "left", "right", "fast-child"],
    );
    assert_lists(
        &trace,
        &["root", "fast", "slow", "left", "right", "fast-child"],
    );
    assert!(child_admitted.get());
    assert_eq!(trace.peak.get(), 4);
    let events = trace.events.borrow();
    let position = |event: &str| events.iter().position(|e| e == event).unwrap();
    assert!(position("complete:fast") < position("list:fast-child"));
    for sibling in ["slow", "left", "right"] {
        assert!(position("list:fast-child") < position(&format!("complete:{sibling}")));
    }
    assert_quiescent(&trace);
    assert!(trace.cancelled.borrow().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn early_failure_cancels_pending_siblings_without_waiting_for_all() {
    let trace = Rc::new(Trace::default());
    let list_trace = trace.clone();
    let list = move |id: String| {
        let mut lease = ListLease::new(list_trace.clone(), id.clone());
        poll_fn(move |cx| {
            lease.start();
            if id == "root" {
                lease.finish();
                return Poll::Ready(Ok(vec![
                    folder("pending", "Pending"),
                    folder("bad", "Bad"),
                    folder("other-a", "A"),
                    folder("other-b", "B"),
                ]));
            }
            if id == "bad"
                && lease
                    .trace
                    .started
                    .borrow()
                    .iter()
                    .any(|id| id == "pending")
            {
                lease.finish();
                return Poll::Ready(Err(PRIVATE_LIST_ERROR));
            }
            cx.waker().wake_by_ref();
            Poll::Pending
        })
    };
    let (value, index) = tokio::time::timeout(
        Duration::from_secs(5),
        parse_indexed_with_callbacks(CONFIG, SHARE_URL, &[], "", list, |_| async {
            Err::<Option<String>, _>(PRIVATE_DOWNLOAD_ERROR)
        }),
    )
    .await
    .expect("an upstream failure must not await pending siblings");
    assert_error(&value, 502);
    assert_eq!(index, None);
    // The service's seeded async wrapper may be dropped before invoking a
    // sibling callback. Do not pretend those unstarted callbacks were leaked.
    let admitted = trace.lists.borrow().clone();
    for id in ["root", "pending", "bad"] {
        assert!(admitted.iter().any(|actual| actual == id));
    }
    assert!(
        admitted
            .iter()
            .all(|id| ["root", "pending", "bad", "other-a", "other-b"].contains(&id.as_str()))
    );
    assert_eq!(
        admitted.len(),
        admitted.iter().collect::<BTreeSet<_>>().len()
    );
    assert_eq!(trace.completed.borrow().as_slice(), ["root", "bad"]);
    let cancelled: BTreeSet<_> = trace.cancelled.borrow().iter().cloned().collect();
    let expected: BTreeSet<_> = admitted
        .into_iter()
        .filter(|id| id != "root" && id != "bad")
        .collect();
    assert_eq!(cancelled, expected);
    assert!(cancelled.contains("pending"));
    assert!(trace.started.borrow().iter().any(|id| id == "pending"));
    assert_quiescent(&trace);
}

#[tokio::test(flavor = "current_thread")]
async fn dropping_a_request_cancels_its_in_flight_callbacks() {
    let trace = Rc::new(Trace::default());
    let list_trace = trace.clone();
    let list = move |id: String| {
        let mut lease = ListLease::new(list_trace.clone(), id.clone());
        poll_fn(move |cx| {
            lease.start();
            if id == "root" {
                lease.finish();
                return Poll::Ready(Ok::<_, &'static str>(vec![
                    folder("a", "A"),
                    folder("b", "B"),
                    folder("c", "C"),
                    folder("d", "D"),
                ]));
            }
            cx.waker().wake_by_ref();
            Poll::Pending
        })
    };
    let mut request = Box::pin(parse_indexed_with_callbacks(
        CONFIG,
        SHARE_URL,
        &[],
        "",
        list,
        |_| async { Ok::<_, &'static str>(None) },
    ));
    tokio::time::timeout(
        Duration::from_secs(5),
        poll_fn(|cx| {
            assert!(request.as_mut().poll(cx).is_pending());
            if trace.active.get() == 4 {
                Poll::Ready(())
            } else {
                // FuturesUnordered may yield cooperatively before polling all slots.
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }),
    )
    .await
    .expect("four child callbacks should eventually be polled");
    assert_eq!(trace.active.get(), 4);
    drop(request);
    assert_lists(&trace, &["root", "a", "b", "c", "d"]);
    assert_eq!(trace.completed.borrow().as_slice(), ["root"]);
    assert_eq!(trace.cancelled.borrow().len(), 4);
    assert_quiescent(&trace);
}

#[test]
fn production_adapter_signatures_are_type_checked() {
    // References only: no network or real account is involved.
    let _ = mcloud_service::parse;
    let _ = mcloud_service::parse_with_fid_chain;
}

#[test]
fn production_indexed_is_send() {
    fn send<T: Send>(_: T) {}
    let client = providers::mcloud_client::MobileCloudClient::new(
        reqwest::Client::new(),
        "synthetic".into(),
    );
    send(mcloud_service::parse_indexed(
        &client,
        CONFIG,
        "invalid",
        "",
        &[],
        "",
    ));
}
