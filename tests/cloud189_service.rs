//! Networkless deployment differential plus explicit read-only/safety differences.
mod support;
pub use jxpan_rust::{compat, tree};
#[path = "../src/providers/cloud189_client.rs"]
mod adapter;
mod providers {
    pub(crate) use super::adapter as cloud189_client;
    pub use jxpan_rust::providers::cloud189;
}
#[path = "../src/providers/cloud189_service.rs"]
mod service;
use serde_json::{Value, json};
use service::DownloadMethod;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

#[tokio::test]
async fn unconfigured_download_does_not_contact_upstream_and_cookies_merge_by_name() {
    let mut client = adapter::Cloud189Client::new(reqwest::Client::new(), json!({}));
    assert!(
        client
            .download_url("synthetic-file", "synthetic-share")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        adapter::merge_cookies("sid=old; keep=1", &["sid=new".into(), "extra=2".into()]),
        "sid=new; keep=1; extra=2"
    );
}

fn oracle() -> Value {
    support::oracle(
        "cloud189-service",
        include_str!("fixtures/cloud189-service.json"),
    )
}
fn method(m: DownloadMethod) -> &'static str {
    match m {
        DownloadMethod::Open => "open",
        DownloadMethod::AppShared => "app_dt1",
        DownloadMethod::AppOwned => "app_dt3",
    }
}
async fn yield_once() {
    let mut yielded = false;
    futures_util::future::poll_fn(move |cx| {
        if yielded {
            std::task::Poll::Ready(())
        } else {
            yielded = true;
            cx.waker().wake_by_ref();
            std::task::Poll::Pending
        }
    })
    .await
}
#[tokio::test]
async fn deployed_read_only_service_differential() {
    let oracle = oracle();
    assert_eq!(
        oracle["source_sha256"],
        "47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118"
    );
    let mut count = 0;
    for vector in oracle["vectors"].as_array().unwrap() {
        count += 1;
        let input = &vector["input"];
        let name = input["name"].as_str().unwrap();
        let trace = Rc::new(RefCell::new(Vec::<Value>::new()));
        let active = Rc::new(Cell::new(0usize));
        let peak = Rc::new(Cell::new(0usize));
        let get = |code: String| {
            trace.borrow_mut().push(json!(["share", code]));
            let info = input.get("info").filter(|v| !v.is_null()).cloned();
            async move { Ok::<_, ()>(info) }
        };
        let check = |code: String, pwd: String| {
            trace.borrow_mut().push(json!(["pwd", code, pwd]));
            let checked = input
                .get("checked")
                .cloned()
                .unwrap_or(json!({"success":true,"shareId":"s"}));
            async move { Ok::<_, ()>(checked) }
        };
        let download = |m: DownloadMethod, id: String, sid: String| {
            trace.borrow_mut().push(json!([
                method(m),
                id,
                if m == DownloadMethod::AppOwned {
                    "".to_owned()
                } else {
                    sid
                }
            ]));
            let url = input["downloads"][method(m)].as_str().map(str::to_owned);
            async move { Ok::<_, ()>(url) }
        };
        let url = input["url"].as_str().unwrap();
        let pwd = input["pwd"].as_str().unwrap();
        if input["fast"] == true {
            let result = service::fast_with_callbacks(
                url,
                pwd,
                input["id"].as_str().unwrap(),
                input["sid"].as_str().unwrap_or(""),
                get,
                check,
                download,
            )
            .await;
            match result {
                Ok(url) => {
                    assert_eq!(vector["expected"]["status"], 302, "{name}");
                    assert_eq!(vector["expected"]["location"], url, "{name}");
                    // Intentional boundary: original Range hop is recorded, not performed by Rust service.
                    let hops: Vec<_> = vector["rawTrace"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|r| r["headers"]["range"] == "bytes=0-0")
                        .collect();
                    assert_eq!(hops.len(), 1, "original fast hop must be exercised: {name}");
                    assert_eq!(hops[0]["redirect"], "manual");
                }
                Err(error) => {
                    let expected: Value =
                        serde_json::from_str(vector["expected"]["body"].as_str().unwrap()).unwrap();
                    assert_eq!(error, expected, "{name}");
                }
            }
        } else {
            let chain: Vec<_> = input["chain"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_owned())
                .collect();
            let list = |sid: String, id: String, mode: String| {
                trace.borrow_mut().push(json!(["list", sid, id, mode]));
                let active = active.clone();
                let peak = peak.clone();
                let rows = input["graph"][&id].as_array().cloned().unwrap_or_default();
                let fail = input["failures"]
                    .as_array()
                    .is_some_and(|a| a.contains(&json!(id)));
                async move {
                    active.set(active.get() + 1);
                    peak.set(peak.get().max(active.get()));
                    yield_once().await;
                    active.set(active.get() - 1);
                    if fail { Err(()) } else { Ok(rows) }
                }
            };
            let result =
                service::parse_with_callbacks(url, pwd, &chain, "", (get, check, list, download))
                    .await;
            if input.get("difference").is_some() {
                assert_eq!(result["code"], 500, "{name}");
                assert_eq!(result["msg"], "获取下载链接失败（只读模式未启用转存回退）");
                assert!(!result.to_string().contains("offline-secret"));
            } else {
                assert_eq!(result, vector["expected"], "{name}");
            }
            assert_eq!(
                peak.get(),
                vector["peak"].as_u64().unwrap() as usize,
                "{name}"
            );
            assert!(peak.get() <= 6, "bounded concurrency");
        }
        assert_eq!(
            json!(*trace.borrow()),
            vector["calls"],
            "method trace: {name}"
        );
        if name == "max-400-concurrency-6" {
            assert_eq!(
                trace.borrow().iter().filter(|c| c[0] == "list").count(),
                400
            );
            assert_eq!(peak.get(), 6);
        }
    }
    assert_eq!(count, 29);
}

#[tokio::test]
async fn targeted_file_lookup_and_directory_rejection_are_explicit_extensions() {
    let root = json!({"fileId":"r","fileName":"Root","fileSize":0,"isFolder":true,"shareId":"s"});
    let calls = RefCell::new(Vec::new());
    for (target, code) in [("f", 200), ("r", 400), ("a", 400), ("missing", 404)] {
        calls.borrow_mut().clear();
        let result = service::parse_with_callbacks(
            "https://cloud.189.cn/t/AbC123",
            "",
            &["a".into()],
            target,
            (
            |_| {
                let root = root.clone();
                async move { Ok::<_, ()>(Some(root)) }
            },
            |_, _| async {
                panic!("password must not be checked");
                #[allow(unreachable_code)]
                Ok::<_, ()>(Value::Null)
            },
            |sid, id, mode| {
                assert_eq!(sid, "s");
                assert_eq!(mode, "1");
                calls.borrow_mut().push(id.clone());
                async move {
                    Ok::<_, ()>(match id.as_str() {
                        "r" => vec![json!({"fileId":"a","fileName":"A","isFolder":true})],
                        "a" => vec![
                            json!({"fileId":"f","fileName":"Film","fileSize":99,"isFolder":false}),
                        ],
                        _ => vec![],
                    })
                }
            },
            |m, id, sid| async move {
                assert_eq!(m, DownloadMethod::Open);
                assert_eq!(id, "f");
                assert_eq!(sid, "s");
                Ok::<_, ()>(Some("https://offline.invalid/file".into()))
            },
            ),
        )
        .await;
        assert_eq!(result["code"], code, "target {target}");
        assert_eq!(
            *calls.borrow(),
            ["r", "a"],
            "no repeated traversal/tree construction"
        );
        if target == "f" {
            assert_eq!(result["data"]["file_name"], "Film");
            assert_eq!(
                result["data"]["download_url"],
                "https://offline.invalid/file"
            );
        }
    }
}
#[tokio::test]
async fn failures_are_sanitized_and_fast_can_acquire_sid_from_pwd_without_info() {
    let result = service::parse_with_callbacks(
        "https://cloud.189.cn/t/AbC123",
        "",
        &[],
        "",
        (
            |_| async { Err::<Option<Value>, _>("synthetic-secret-should-not-leak") },
            |_, _| async { Ok::<_, &str>(Value::Null) },
            |_, _, _| async { Ok::<_, &str>(vec![]) },
            |_, _, _| async { Ok::<_, &str>(None) },
        ),
    )
    .await;
    assert_eq!(result["code"], 500);
    assert!(!result.to_string().contains("synthetic-secret"));
    let result = service::fast_with_callbacks(
        "https://cloud.189.cn/t/AbC123",
        "pwd",
        "f",
        "",
        |_| async { Ok::<_, ()>(None) },
        |_, _| async { Ok::<_, ()>(json!({"success":true,"shareId":"pwd-sid"})) },
        |m, _, sid| async move {
            assert_eq!(sid, "pwd-sid");
            assert_eq!(m, DownloadMethod::AppShared);
            Ok::<_, ()>(Some("https://offline.invalid/file".into()))
        },
    )
    .await;
    assert_eq!(result.unwrap(), "https://offline.invalid/file");
}

// Loopback-only HTTP transcript test of the actual production adapter; no public endpoint override.
fn stub(
    responses: Vec<(Value, Vec<&'static str>)>,
) -> (String, std::thread::JoinHandle<Vec<Value>>) {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
        time::{Duration, Instant},
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut requests = vec![];
        for (body, cookies) in responses {
            let mut stream = loop {
                match listener.accept() {
                    Ok((s, _)) => break s,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(1))
                    }
                    Err(_) => panic!("loopback stub deadline"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = vec![];
            let end = loop {
                let mut b = [0u8; 4096];
                let n = stream.read(&mut b).unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&b[..n]);
                assert!(bytes.len() < 128 * 1024);
                if let Some(pos) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    break pos + 4;
                }
            };
            let head = String::from_utf8(bytes[..end].to_vec()).unwrap();
            let mut lines = head.split("\r\n");
            let first: Vec<_> = lines.next().unwrap().split_whitespace().collect();
            let mut headers = serde_json::Map::new();
            for line in lines.filter(|s| !s.is_empty()) {
                let (k, v) = line.split_once(':').unwrap();
                headers.insert(k.to_ascii_lowercase(), json!(v.trim()));
            }
            let length = headers
                .get("content-length")
                .and_then(Value::as_str)
                .unwrap_or("0")
                .parse::<usize>()
                .unwrap();
            while bytes.len() - end < length {
                let mut b = [0u8; 4096];
                let n = stream.read(&mut b).unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&b[..n]);
            }
            for key in ["host", "content-length", "connection", "accept-encoding"] {
                headers.remove(key);
            }
            requests.push(json!({"method":first[0],"target":first[1],"headers":headers}));
            let body = body.to_string();
            let mut head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
                body.len()
            );
            for cookie in cookies {
                head.push_str(&format!("Set-Cookie: {cookie}\r\n"));
            }
            stream
                .write_all(format!("{head}\r\n{body}").as_bytes())
                .unwrap();
        }
        requests
    });
    (base, handle)
}
fn loopback_client(base: &str, credentials: Value) -> adapter::Cloud189Client {
    adapter::Cloud189Client::for_loopback_test(
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .unwrap(),
        credentials,
        base,
    )
}
#[tokio::test]
async fn captured_password_cookies_are_cloned_into_all_directory_clients() {
    let mut responses = vec![
        (
            json!({"res_code":0,"fileId":"r","fileName":"Root","isFolder":1,"shareId":"old"}),
            vec![],
        ),
        (
            json!({"res_code":0,"shareId":"new"}),
            vec!["SHARE_SESSION=synthetic; Path=/"],
        ),
        (
            json!({"res_code":0,"folderList":(0..6).map(|i|json!({"id":format!("d{i}"),"name":format!("Dir{i}")})).collect::<Vec<_>>() }),
            vec![],
        ),
    ];
    responses.extend((0..6).map(|_| (json!({"res_code":0}), vec![])));
    responses.push((json!({"res_code":0}), vec![]));
    let (base, handle) = stub(responses);
    let client = loopback_client(&base, json!({}));
    let mut pristine = client.clone();
    let result = service::parse(client, "https://cloud.189.cn/t/AbC123", "pwd", &[], "").await;
    assert_eq!(result["data"]["sid"], "new");
    pristine
        .list_share_dir("s", "other", "", "1")
        .await
        .unwrap();
    let requests = handle.join().unwrap();
    assert_eq!(requests.len(), 10);
    for req in &requests[2..9] {
        assert_eq!(req["headers"]["cookie"], "SHARE_SESSION=synthetic");
        assert!(req["target"].as_str().unwrap().contains("shareId=new"));
    }
    // A pre-capture clone cannot acquire cookies through reqwest/global jars.
    assert!(requests[9]["headers"]["cookie"].is_null());
    // No signed credentials means public download primitives perform no API call.
    assert_eq!(pristine.download_open("f", "s").await.unwrap(), None);
    assert_eq!(pristine.download_app("f", "s", true).await.unwrap(), None);
}
#[tokio::test]
async fn fast_actual_http_matches_original_raw_trace_except_range_hop() {
    let oracle = oracle();
    for vector in oracle["vectors"].as_array().unwrap().iter().filter(|v| {
        matches!(
            v["input"]["name"].as_str().unwrap(),
            "fast-app-dt1" | "fast-app-dt3" | "fast-open-fallback"
        )
    }) {
        let input = &vector["input"];
        let responses = vector["calls"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| {
                let key = c[0].as_str().unwrap();
                if !["app_dt1", "app_dt3", "open"].contains(&key) {
                    return None;
                }
                Some((
                    input["downloads"]
                        .get(key)
                        .map(|u| json!({"fileDownloadUrl":u}))
                        .unwrap_or(json!({"res_code":403})),
                    vec![],
                ))
            })
            .collect();
        let (base, handle) = stub(responses);
        let client = loopback_client(
            &base,
            json!({"session_key":"offline-session","session_secret":"offline-secret","open_access_token":"offline-open-token-123456789"}),
        );
        let url = service::fast_download(client, "invalid", "wrong", "f", "s")
            .await
            .unwrap();
        assert_eq!(url, vector["expected"]["location"]);
        let actual = handle.join().unwrap();
        let expected:Vec<_>=vector["rawTrace"].as_array().unwrap().iter().filter(|r|r["headers"]["range"].is_null()).map(|r| {
            let u=url::Url::parse(r["url"].as_str().unwrap()).unwrap();
            json!({"method":r["method"],"target":format!("{}?{}",u.path(),u.query().unwrap()),"headers":r["headers"]})
        }).collect();
        assert_eq!(actual, expected, "raw trace {}", input["name"]);
    }
}
