//! Networkless deployment differential plus explicit read-only/safety differences.
mod support;
pub use jxpan_rust::{compat, settings, tree};
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
    if m == DownloadMethod::Open {
        "open"
    } else {
        assert_eq!(
            m,
            DownloadMethod::AppShared,
            "owned/other methods are forbidden in public callbacks"
        );
        "app_dt1"
    }
}
// The frozen VM remains the legacy oracle, including unsafe dt=3 successes.
// Public expectations are independent: two shared methods, in business order,
// stopping at the first nonempty URL. Merely filtering dt=3 from a frozen trace
// is insufficient when the legacy fast branch stopped before reaching Open.
fn expected_public_calls(vector: &Value, fast: bool) -> Value {
    let frozen = vector["calls"].as_array().unwrap();
    let is_download =
        |call: &&Value| matches!(call[0].as_str(), Some("open" | "app_dt1" | "app_dt3"));
    let mut calls: Vec<_> = frozen.iter().filter(|c| !is_download(c)).cloned().collect();
    if let Some(first) = frozen
        .iter()
        .find(|c| matches!(c[0].as_str(), Some("open" | "app_dt1")))
    {
        let methods = if fast {
            ["app_dt1", "open"]
        } else {
            ["open", "app_dt1"]
        };
        for key in methods {
            calls.push(json!([key, first[1], first[2]]));
            if vector["input"]["downloads"][key]
                .as_str()
                .is_some_and(|url| !url.is_empty())
            {
                break;
            }
        }
    }
    json!(calls)
}
fn public_error(code: u16, msg: &str) -> Value {
    json!({"code":code,"msg":msg,"success":false,"data":null})
}
fn password_error() -> Value {
    public_error(403, "提取码错误或已过期，请检查提取码是否正确")
}
// Frozen bug-oracle data stays intact. Only these independent public expectations change.
fn expected_service_response(vector: &Value) -> Value {
    match vector["input"]["name"].as_str().unwrap() {
        "missing-fid-fallback" => public_error(404, "未找到指定文件夹"),
        "failed-child-is-empty" => public_error(502, "天翼目录列表获取失败"),
        "duplicates-collisions" => public_error(502, "天翼目录元数据无效"),
        "zero-size-treated-as-directory" | "single-app-dt3" | "ordinary-no-direct" => {
            public_parse_download_failure()
        }
        "fast-zero-size-rejected" | "fast-app-dt3" => public_fast_download_failure(),
        "fast-existing-sid-skips-share-pwd" => public_error(502, "无效分享链接"),
        "fast-password-failure-retains-sid" => password_error(),
        "max-400-concurrency-6" => {
            let folders: serde_json::Map<_, _> =
                (0..450).map(|i| (format!("d{i}"), json!({}))).collect();
            json!({"code":200,"msg":"文件夹列表获取成功","success":true,"shareKey":"c189:s",
                "data":{"tree":{"共享":folders},"sid":"s"}})
        }
        _ if vector["input"]["fast"] == true => {
            serde_json::from_str(vector["expected"]["body"].as_str().unwrap())
                .unwrap_or_else(|error| panic!("{}: {error}", vector["input"]["name"]))
        }
        _ => vector["expected"].clone(),
    }
}
fn expected_service_calls(vector: &Value) -> Value {
    let mut calls = expected_public_calls(vector, vector["input"]["fast"] == true);
    match vector["input"]["name"].as_str().unwrap() {
        "zero-size-treated-as-directory" => {
            calls = json!([
                ["share", "AbC123"],
                ["open", "f", "s"],
                ["app_dt1", "f", "s"]
            ])
        }
        "fast-zero-size-rejected" => {
            calls = json!([
                ["share", "AbC123"],
                ["app_dt1", "f", "s"],
                ["open", "f", "s"]
            ])
        }
        "fast-existing-sid-skips-share-pwd" => calls = json!([]),
        "fast-password-failure-retains-sid" => calls.as_array_mut().unwrap().truncate(2),
        "duplicates-collisions" => calls.as_array_mut().unwrap().truncate(2),
        "max-400-concurrency-6" => {
            let mut complete = vec![json!(["share", "AbC123"]), json!(["list", "s", "r", "1"])];
            complete.extend((0..450).map(|i| json!(["list", "s", format!("d{i}"), "1"])));
            calls = json!(complete);
        }
        _ => {}
    }
    calls
}
// Independent expected adaptation only: the frozen fast-folder-download-endpoint
// vector rejected a normal signed file trampoline by URL substring. Preserve
// that bug-oracle and its raw trace unchanged; only the public expectation differs.
fn expected_fast_response(vector: &Value) -> Value {
    if vector["input"]["name"] == "fast-folder-download-endpoint" {
        assert_eq!(
            vector["expected"]["status"], 400,
            "frozen URL-heuristic rejection"
        );
        let url = ["app_dt1", "open"]
            .into_iter()
            .find_map(|key| {
                vector["input"]["downloads"][key]
                    .as_str()
                    .filter(|url| !url.is_empty())
            })
            .expect("frozen shared file trampoline");
        assert!(url.starts_with("https://download.cloud.189.cn/file/downloadFile.action"));
        json!({"status":302,"location":url})
    } else {
        vector["expected"].clone()
    }
}
fn public_parse_download_failure() -> Value {
    json!({"code":500,"msg":"获取下载链接失败（只读模式未启用转存回退）","success":false,"data":null})
}
fn public_file_download_failure(id: &str, name: &str, size: Value) -> Value {
    json!({"code":500,"msg":"下载链接获取失败（只读模式未启用转存回退）","success":false,
        "data":{"type":"file","fileName":name,"fileSize":size,"fileId":id,"downloadUrl":null}})
}
fn public_fast_download_failure() -> Value {
    json!({"code":502,"msg":"无法获取直链","success":false,"data":null})
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
            trace.borrow_mut().push(json!([method(m), id, sid]));
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
                    assert_ne!(
                        name, "fast-app-dt3",
                        "owned-only frozen URL must never succeed publicly"
                    );
                    let expected = expected_fast_response(vector);
                    assert_eq!(expected["status"], 302, "{name}");
                    assert_eq!(expected["location"], url, "{name}");
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
                Err(error) if name == "fast-app-dt3" => {
                    assert_eq!(vector["expected"]["status"], 302, "frozen owned success");
                    assert!(input["downloads"]["open"].is_null());
                    assert!(input["downloads"]["app_dt1"].is_null());
                    assert_eq!(
                        error,
                        public_fast_download_failure(),
                        "security divergence: {name}"
                    );
                }
                Err(error) => {
                    assert_eq!(error, expected_service_response(vector), "{name}");
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
            if name == "single-app-dt3" {
                assert_eq!(vector["expected"]["code"], 200, "frozen owned success");
                assert_eq!(vector["expected"]["success"], true);
                assert!(input["downloads"]["open"].is_null());
                assert!(input["downloads"]["app_dt1"].is_null());
                assert_eq!(
                    result,
                    public_parse_download_failure(),
                    "security divergence: {name}"
                );
            } else if input.get("difference").is_some() {
                assert_eq!(
                    result,
                    public_parse_download_failure(),
                    "read-only difference: {name}"
                );
                assert!(!result.to_string().contains("offline-secret"));
            } else {
                assert_eq!(result, expected_service_response(vector), "{name}");
            }
            let expected_peak = match name {
                "zero-size-treated-as-directory" => 0,
                "max-400-concurrency-6" => settings::DEFAULT_CONCURRENCY,
                _ => vector["peak"].as_u64().unwrap() as usize,
            };
            assert_eq!(peak.get(), expected_peak, "{name}");
            assert!(
                peak.get() <= settings::DEFAULT_CONCURRENCY,
                "bounded concurrency"
            );
        }
        let expected_calls = expected_service_calls(vector);
        assert_eq!(
            json!(*trace.borrow()),
            expected_calls,
            "public method trace: {name}"
        );
        assert!(
            !trace.borrow().iter().any(|c| c[0] == "app_dt3"),
            "no owned fallback: {name}"
        );
        if name == "max-400-concurrency-6" {
            assert_eq!(
                trace.borrow().iter().filter(|c| c[0] == "list").count(),
                451
            );
            assert_eq!(peak.get(), settings::DEFAULT_CONCURRENCY);
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
        // After the exact script, keep a short, bounded canary window. An
        // unexpected third request is recorded and answered with an owned-URL bait,
        // so swallowed connection errors cannot hide an attempted owned fallback.
        for response in responses.into_iter().map(Some).chain(std::iter::once(None)) {
            let unexpected = response.is_none();
            let wait_deadline = if unexpected {
                Instant::now() + Duration::from_millis(100)
            } else {
                deadline
            };
            let mut stream = loop {
                match listener.accept() {
                    Ok((s, _)) => break s,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < wait_deadline =>
                    {
                        thread::sleep(Duration::from_millis(1))
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && unexpected => {
                        return requests;
                    }
                    Err(_) => panic!("loopback stub deadline"),
                }
            };
            let (body, cookies) = response.unwrap_or((
                json!({"fileDownloadUrl":"https://offline.invalid/forbidden-owned"}),
                vec![],
            ));
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
            requests.push(
                json!({"method":first[0],"target":first[1],"headers":headers,
                "body":std::str::from_utf8(&bytes[end..end + length]).unwrap()}),
            );
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
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap(),
        credentials,
        base,
    )
    .with_limit(settings::UpstreamLimit::new(2))
}
#[tokio::test]
async fn captured_password_cookies_are_cloned_into_all_directory_clients() {
    let mut responses = vec![
        (
            // Real password-gated shares withhold metadata until verification.
            json!({"res_code":403}),
            vec![],
        ),
        (
            json!({"res_code":0,"shareId":"new"}),
            vec!["SHARE_SESSION=synthetic; Path=/"],
        ),
        (
            json!({"res_code":0,"fileId":"r","fileName":"Root","isFolder":1,"shareId":"old"}),
            vec![],
        ),
        (
            json!({"res_code":0,"folderList":(0..6).map(|i|json!({"id":format!("d{i}"),"name":format!("Dir{i}")})).collect::<Vec<_>>() }),
            vec![],
        ),
    ];
    responses.extend((0..6).map(|_| (json!({"res_code":0,"fileList":[]}), vec![])));
    responses.push((json!({"res_code":0,"fileList":[]}), vec![]));
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
    assert_eq!(requests.len(), 11);
    assert!(requests[0]["headers"]["cookie"].is_null());
    assert!(
        requests[1]["target"]
            .as_str()
            .unwrap()
            .starts_with("/open/share/checkAccessCode.action")
    );
    assert!(
        requests[2]["target"]
            .as_str()
            .unwrap()
            .starts_with("/open/share/getShareInfoByCodeV2.action")
    );
    assert_eq!(
        requests[2]["headers"]["cookie"], "SHARE_SESSION=synthetic",
        "metadata retry must send the password check's captured cookie"
    );
    for req in &requests[3..10] {
        assert_eq!(req["headers"]["cookie"], "SHARE_SESSION=synthetic");
        assert!(req["target"].as_str().unwrap().contains("shareId=new"));
    }
    // A pre-capture clone cannot acquire cookies through reqwest/global jars.
    assert!(requests[10]["headers"]["cookie"].is_null());
    // No signed credentials means public download primitives perform no API call.
    assert_eq!(pristine.download_open("f", "s").await.unwrap(), None);
    assert_eq!(pristine.download_app("f", "s", true).await.unwrap(), None);
}
fn assert_public_download_requests(requests: &[Value], id: &str, sid: &str, methods: &[&str]) {
    assert_eq!(
        requests.len(),
        methods.len(),
        "only the public attempts are allowed"
    );
    for (request, expected_method) in requests.iter().zip(methods) {
        assert_eq!(request["method"], "GET");
        let url = url::Url::parse(&format!(
            "http://127.0.0.1{}",
            request["target"].as_str().unwrap()
        ))
        .unwrap();
        let method = match url.path() {
            "/open/file/getFileDownloadUrl.action" => "open",
            "/getFileDownloadUrl.action" => "app_dt1",
            path => panic!("unexpected public download endpoint: {path}"),
        };
        assert_eq!(method, *expected_method);
        let params: std::collections::HashMap<_, _> = url.query_pairs().collect();
        assert_eq!(
            params.get("dt").map(|v| v.as_ref()),
            Some("1"),
            "never owned dt=3"
        );
        assert_eq!(
            params.get("shareId").map(|v| v.as_ref()),
            Some(sid),
            "shareId must be retained"
        );
        assert_eq!(params.get("fileId").map(|v| v.as_ref()), Some(id));
        assert!(!params.contains_key("flag"), "no owned flag=1");
        assert!(
            request["headers"]["range"].is_null(),
            "service must not fetch download data"
        );
    }
}

#[tokio::test]
async fn fast_actual_http_matches_public_methods_and_explicit_owned_divergence() {
    let oracle = oracle();
    // The legacy owned-success fast branch never reached Open. Its new second
    // request is compared to the same frozen corpus's real Open fallback request,
    // with identical synthetic fileId/shareId/credentials/time, not to a fabricated trace.
    let open_fallback = oracle["vectors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["input"]["name"] == "fast-open-fallback")
        .unwrap();
    for vector in oracle["vectors"].as_array().unwrap().iter().filter(|v| {
        matches!(
            v["input"]["name"].as_str().unwrap(),
            "fast-app-dt1" | "fast-app-dt3" | "fast-open-fallback"
        )
    }) {
        let input = &vector["input"];
        let calls = expected_public_calls(vector, true);
        let methods: Vec<_> = calls
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| {
                c[0].as_str()
                    .filter(|key| ["app_dt1", "open"].contains(key))
            })
            .collect();
        let responses = methods
            .iter()
            .map(|key| {
                (
                    input["downloads"]
                        .get(*key)
                        .map(|u| json!({"fileDownloadUrl":u}))
                        .unwrap_or(json!({"res_code":403})),
                    vec![],
                )
            })
            .collect();
        // Stub count follows the public sequence, never the original dt=3 count.
        let (base, handle) = stub(responses);
        let client = loopback_client(
            &base,
            json!({"session_key":"offline-session","session_secret":"offline-secret","open_access_token":"offline-open-token-123456789"}),
        );
        let result = service::fast_download(client, "invalid", "", "f", "s").await;
        let actual = handle.join().unwrap();
        if input["name"] == "fast-app-dt3" {
            assert_eq!(vector["expected"]["status"], 302, "frozen owned success");
            assert_eq!(
                result.unwrap_err(),
                public_fast_download_failure(),
                "owned success is not public success"
            );
        } else {
            assert_eq!(result.unwrap(), vector["expected"]["location"]);
        }
        assert_public_download_requests(&actual, "f", "s", &methods);
        let expected: Vec<_> = methods.iter().map(|key| {
            let raw = vector["rawTrace"].as_array().unwrap().iter()
                .chain(open_fallback["rawTrace"].as_array().unwrap())
                .find(|r| {
                    if !r["headers"]["range"].is_null() { return false; }
                    let u = url::Url::parse(r["url"].as_str().unwrap()).unwrap();
                    let path = if *key == "open" { "/open/file/getFileDownloadUrl.action" } else { "/getFileDownloadUrl.action" };
                    u.path() == path
                        && u.query_pairs().any(|(k, v)| k == "dt" && v == "1")
                        && u.query_pairs().any(|(k, v)| k == "shareId" && v == "s")
                }).expect("frozen shared-method transcript");
            let u = url::Url::parse(raw["url"].as_str().unwrap()).unwrap();
            json!({"method":raw["method"],"target":format!("{}?{}",u.path(),u.query().unwrap()),"headers":raw["headers"]})
        }).map(|mut request| { request["body"] = json!(""); request }).collect();
        assert_eq!(actual, expected, "public raw trace {}", input["name"]);
    }
}

#[tokio::test]
async fn frozen_file_and_folder_action_differential() {
    let corpus = support::oracle(
        "cloud189-action",
        include_str!("fixtures/cloud189-action.json"),
    );
    assert_eq!(corpus["vectors"].as_array().unwrap().len(), 20);
    for vector in corpus["vectors"].as_array().unwrap() {
        let input = &vector["input"];
        let name = input["name"].as_str().unwrap();
        let trace = RefCell::new(Vec::<Value>::new());
        let check = |code, pwd| {
            trace.borrow_mut().push(json!(["pwd", code, pwd]));
            let result = input
                .get("checked")
                .cloned()
                .unwrap_or(json!({"success":true}));
            async move { Ok::<_, ()>(result) }
        };
        let s = |key: &str| input[key].as_str().unwrap();
        let size = if input["huge_size"] == true {
            "9".repeat(400)
        } else {
            s("file_size").to_owned()
        };
        let actual = if s("action") == "file" {
            service::file_action_with_callbacks(
                s("share_id"),
                s("file_id"),
                s("file_name"),
                &size,
                s("pwd"),
                s("share_code"),
                check,
                |m, id, sid| {
                    trace.borrow_mut().push(json!([method(m), id, sid]));
                    let url = input["downloads"][method(m)].as_str().map(str::to_owned);
                    async move { Ok::<_, ()>(url) }
                },
            )
            .await
        } else {
            service::folder_action_with_callbacks(
                s("share_id"),
                s("folder_id"),
                s("target_file_id"),
                s("pwd"),
                s("share_mode"),
                s("share_code"),
                check,
                |sid, id, mode| {
                    trace.borrow_mut().push(json!(["list", sid, id, mode]));
                    let rows = input["graph"][&id].as_array().cloned().unwrap_or_default();
                    let fail = input["failures"]
                        .as_array()
                        .is_some_and(|a| a.contains(&json!(id)));
                    async move { if fail { Err(()) } else { Ok(rows) } }
                },
            )
            .await
        };
        if name == "app-owned" {
            assert_eq!(vector["expected"]["code"], 200, "frozen owned success");
            assert_eq!(vector["expected"]["success"], true);
            assert!(input["downloads"]["open"].is_null());
            assert!(input["downloads"]["app_dt1"].is_null());
            assert_eq!(
                actual,
                public_file_download_failure("f", "", json!(0)),
                "security divergence: {name}"
            );
        } else if input.get("difference").is_some() {
            assert_eq!(vector["expected"]["code"], 500, "frozen transfer failure");
            assert_eq!(vector["expected"]["msg"], "下载链接获取失败");
            assert_eq!(
                actual,
                public_file_download_failure("f", "", json!(0)),
                "read-only difference: {name}"
            );
        } else {
            let expected = match name {
                "open-encoded" | "negative-prefix" | "invalid-size" => {
                    let mut expected = vector["expected"].clone();
                    expected["data"]["fileSize"] = Value::Null;
                    expected["data"]["downloadUrl"] = input["downloads"]["open"].clone();
                    expected
                }
                "decimal-large" => {
                    let mut expected = vector["expected"].clone();
                    expected["data"]["fileSize"] = json!("9007199254740993");
                    expected
                }
                "password-failure-ignored" | "folder-password-mode" => password_error(),
                "folder-list-failure" => public_error(502, "天翼目录列表获取失败"),
                "folder-existing-file-no-idmap" => {
                    public_file_download_failure("f", "film", json!(99))
                }
                "folder-existing-folder-no-idmap" => {
                    json!({"code":200,"msg":"文件夹列表获取成功","success":true,
                    "shareKey":"c189:s","data":{"tree":{"a":{}},"sid":"s"}})
                }
                _ => vector["expected"].clone(),
            };
            assert_eq!(actual, expected, "response {name}");
        }
        let mut expected_calls = expected_public_calls(vector, false);
        if matches!(name, "password-failure-ignored" | "folder-password-mode") {
            expected_calls.as_array_mut().unwrap().truncate(1);
        }
        assert_eq!(
            json!(*trace.borrow()),
            expected_calls,
            "public sequence {name}"
        );
        assert!(
            !trace.borrow().iter().any(|c| c[0] == "app_dt3"),
            "no owned fallback: {name}"
        );
    }
}

#[tokio::test]
async fn arbitrary_public_ids_never_fall_back_to_owned_callbacks() {
    const URL: &str = "https://offline.invalid/shared-only";
    // IDs are not proof of ownership or membership in a share. Include a made-up
    // numeric pair as well as plainly fake names, without contacting any cloud.
    for (sid, id) in [
        ("fake-share-not-authorized", "private-file-candidate"),
        ("987654321", "123456789"),
    ] {
        for reply in [
            "missing",
            "empty",
            "error",
            "open-success",
            "shared-success",
        ] {
            for route in ["parse", "targeted-parse", "file", "fast"] {
                let calls = RefCell::new(Vec::<Value>::new());
                let download = |m: DownloadMethod, file_id: String, share_id: String| {
                    assert_eq!(file_id, id);
                    assert_eq!(share_id, sid);
                    calls
                        .borrow_mut()
                        .push(json!([method(m), file_id, share_id]));
                    let result = match reply {
                        "empty" => Ok(Some(String::new())),
                        "error" => Err("synthetic-download-secret-must-not-leak"),
                        "open-success" if m == DownloadMethod::Open => Ok(Some(URL.to_owned())),
                        "shared-success" if m == DownloadMethod::AppShared => {
                            Ok(Some(URL.to_owned()))
                        }
                        _ => Ok(None),
                    };
                    async move { result }
                };
                let success = matches!(reply, "open-success" | "shared-success");
                if route == "fast" {
                    let result = service::fast_with_callbacks(
                        "invalid",
                        "",
                        id,
                        sid,
                        |_| async {
                            panic!("existing sid bypasses share lookup");
                            #[allow(unreachable_code)]
                            Ok::<_, &str>(None)
                        },
                        |_, _| async {
                            panic!("existing sid bypasses password lookup");
                            #[allow(unreachable_code)]
                            Ok::<_, &str>(Value::Null)
                        },
                        download,
                    )
                    .await;
                    if success {
                        assert_eq!(result.unwrap(), URL);
                    } else if reply == "error" {
                        assert_eq!(
                            result.unwrap_err(),
                            public_error(502, "天翼下载链接请求失败")
                        );
                    } else {
                        assert_eq!(result.unwrap_err(), public_fast_download_failure());
                    }
                } else {
                    let result = if route == "file" {
                        service::file_action_with_callbacks(
                            sid,
                            id,
                            "synthetic.mp4",
                            "7",
                            "",
                            "",
                            |_, _| async {
                                panic!("no password lookup");
                                #[allow(unreachable_code)]
                                Ok::<_, &str>(Value::Null)
                            },
                            download,
                        )
                        .await
                    } else {
                        let file = json!({"fileId":id,"fileName":"synthetic.mp4","fileSize":7,"isFolder":false,"shareId":sid});
                        let targeted = route == "targeted-parse";
                        let info = if targeted {
                            json!({"fileId":"synthetic-root","fileName":"Root","fileSize":0,"isFolder":true,"shareId":sid})
                        } else {
                            file.clone()
                        };
                        service::parse_with_callbacks(
                            "https://cloud.189.cn/t/AbC123",
                            "",
                            &[],
                            if targeted { id } else { "" },
                            (
                                |_| {
                                    let info = info.clone();
                                    async move { Ok::<_, &str>(Some(info)) }
                                },
                                |_, _| async {
                                    panic!("no password lookup");
                                    #[allow(unreachable_code)]
                                    Ok::<_, &str>(Value::Null)
                                },
                                |share_id, root, _| {
                                    assert!(targeted, "single-file parse must not walk");
                                    assert_eq!(share_id, sid);
                                    assert_eq!(root, "synthetic-root");
                                    let file = file.clone();
                                    async move { Ok::<_, &str>(vec![file]) }
                                },
                                download,
                            ),
                        )
                        .await
                    };
                    if success {
                        assert_eq!(result["code"], 200, "{route}/{reply}");
                        assert_eq!(result["success"], true);
                        if route == "file" {
                            assert_eq!(result["data"]["downloadUrl"], URL);
                        } else {
                            assert_eq!(result["data"]["download_url"], URL);
                        }
                    } else if reply == "error" {
                        assert_eq!(result, public_error(502, "天翼下载链接请求失败"), "{route}");
                    } else if route == "file" {
                        assert_eq!(
                            result,
                            public_file_download_failure(id, "synthetic.mp4", json!(7)),
                            "{route}/{reply}"
                        );
                    } else {
                        assert_eq!(result, public_parse_download_failure(), "{route}/{reply}");
                    }
                    assert!(!result.to_string().contains("synthetic-download-secret"));
                }
                let mut methods = if route == "fast" {
                    vec!["app_dt1", "open"]
                } else {
                    vec!["open", "app_dt1"]
                };
                if (reply == "open-success" && route != "fast")
                    || (reply == "shared-success" && route == "fast")
                {
                    methods.truncate(1);
                }
                let expected: Vec<_> = methods.iter().map(|key| json!([key, id, sid])).collect();
                assert_eq!(*calls.borrow(), expected, "public sequence {route}/{reply}");
            }
        }
    }
}

#[tokio::test]
async fn arbitrary_public_ids_actual_http_exhaust_shared_methods_without_owned() {
    const URL: &str = "https://offline.invalid/shared-open";
    for (sid, id) in [
        ("fake-share-not-authorized", "private-file-candidate"),
        ("987654321", "123456789"),
    ] {
        for route in ["parse", "targeted-parse", "file", "fast"] {
            for open_url in [None, Some(URL)] {
                let mut responses = vec![];
                if route == "parse" {
                    responses.push((json!({"res_code":0,"fileId":id,"fileName":"synthetic.mp4","fileSize":7,"isFolder":0,"shareId":sid}), vec![]));
                } else if route == "targeted-parse" {
                    responses.push((json!({"res_code":0,"fileId":"synthetic-root","fileName":"Root","fileSize":0,"isFolder":1,"shareId":sid}), vec![]));
                    responses.push((json!({"res_code":0,"fileList":[{"id":id,"name":"synthetic.mp4","size":7}]}), vec![]));
                } else if route == "file" {
                    responses.push((json!({"res_code":0,"fileList":[{"id":id,"name":"synthetic.mp4","size":7}]}), vec![]));
                }
                let prefix_len = responses.len();
                let methods: &[&str] = if route == "fast" {
                    &["app_dt1", "open"]
                } else if open_url.is_some() {
                    &["open"]
                } else {
                    &["open", "app_dt1"]
                };
                responses.extend(methods.iter().map(|key| (
                    // Explicitly empty URLs, not only missing keys. No third owned response.
                    json!({"fileDownloadUrl":if *key == "open" { open_url.unwrap_or("") } else { "" }}), vec![],
                )));
                let (base, handle) = stub(responses);
                let client = loopback_client(
                    &base,
                    json!({"session_key":"offline-session","session_secret":"offline-secret","open_access_token":"offline-open-token-123456789"}),
                );
                let result = match route {
                    "parse" => Some(
                        service::parse(client, "https://cloud.189.cn/t/AbC123", "", &[], "").await,
                    ),
                    "targeted-parse" => Some(
                        service::parse(client, "https://cloud.189.cn/t/AbC123", "", &[], id).await,
                    ),
                    "file" => Some(service::parse_file(client, sid, id, "", "7", "", "").await),
                    "fast" => {
                        let result = service::fast_download(client, "invalid", "", id, sid).await;
                        if let Some(url) = open_url {
                            assert_eq!(result.unwrap(), url);
                        } else {
                            assert_eq!(result.unwrap_err(), public_fast_download_failure());
                        }
                        None
                    }
                    _ => unreachable!(),
                };
                let requests = handle.join().unwrap();
                assert_public_download_requests(&requests[prefix_len..], id, sid, methods);
                if let Some(result) = result {
                    if let Some(url) = open_url {
                        assert_eq!(result["code"], 200, "{route}");
                        assert_eq!(result["success"], true);
                        let key = if route == "file" {
                            "downloadUrl"
                        } else {
                            "download_url"
                        };
                        assert_eq!(result["data"][key], url);
                    } else if route == "file" {
                        assert_eq!(
                            result,
                            public_file_download_failure(id, "synthetic.mp4", json!(7))
                        );
                    } else {
                        assert_eq!(result, public_parse_download_failure());
                    }
                }
            }
        }
    }
}

#[tokio::test]
async fn indexed_walk_retains_root_collisions_and_unprojected_ids_once() {
    let calls = RefCell::new(Vec::new());
    let (result, index) = service::parse_indexed_with_callbacks(
        "https://cloud.189.cn/t/AbC123",
        "",
        &["a".into()],
        "",
        (
            |_| async {
                Ok::<_, ()>(Some(
                    json!({"fileId":"r","fileName":"R","shareId":"s","isFolder":true}),
                ))
            },
            |_, _| async { Ok::<_, ()>(Value::Null) },
            |_, id, _| {
                calls.borrow_mut().push(id.clone());
                async move {
                    Ok::<_, ()>(match id.as_str() {
                        "r" => vec![
                            json!({"fileId":"a","fileName":"same","isFolder":true}),
                            json!({"fileId":"hidden","fileName":"same","isFolder":true}),
                            json!({"fileId":"f","fileName":"file","fileSize":99}),
                        ],
                        "hidden" => vec![json!({"fileId":"empty","fileName":"E","isFolder":true})],
                        _ => vec![],
                    })
                }
            },
            |_, _, _| async { Ok::<_, ()>(None) },
        ),
    )
    .await;
    assert_eq!(
        index,
        Some((
            "s".into(),
            vec!["r".into(), "a".into(), "hidden".into(), "empty".into()]
        ))
    );
    assert_eq!(*calls.borrow(), ["r", "a", "hidden", "empty"]);
    assert_eq!(result["data"]["tree"], json!({"R/same":{}}));
}

#[tokio::test]
async fn zero_and_unknown_size_files_are_never_inferred_to_be_directories() {
    const URL: &str = "https://offline.invalid/file?signature=a%2Bb%3D&x=1#part";
    for (input_size, expected) in [
        (Some(json!(0)), json!(0)),
        (Some(json!("0")), json!("0")),
        (None, Value::Null),
        (Some(json!(9007199254740993u64)), json!("9007199254740993")),
        (Some(json!(u64::MAX)), json!(u64::MAX.to_string())),
    ] {
        let mut info =
            json!({"fileId":"f","fileName":"zero.txt","isFolder":false,"isDir":true,"shareId":"s"});
        if let Some(size) = input_size {
            info["fileSize"] = size;
        }
        let result = service::parse_with_callbacks(
            "https://cloud.189.cn/t/AbC123",
            "",
            &[],
            "",
            (
                |_| {
                    let info = info.clone();
                    async move { Ok::<_, ()>(Some(info)) }
                },
                |_, _| async {
                    panic!("no password");
                    #[allow(unreachable_code)]
                    Ok::<_, ()>(Value::Null)
                },
                |_, _, _| async {
                    panic!("zero/unknown size is a file, not a listing");
                    #[allow(unreachable_code)]
                    Ok::<_, ()>(vec![])
                },
                |m, id, sid| async move {
                    assert_eq!(
                        (m, id.as_str(), sid.as_str()),
                        (DownloadMethod::Open, "f", "s")
                    );
                    Ok::<_, ()>(Some(URL.into()))
                },
            ),
        )
        .await;
        assert_eq!(result["success"], true);
        assert_eq!(result["data"]["file_size"], expected);
        assert_eq!(result["data"]["files"][0]["size"], expected);
        assert_eq!(result["data"]["download_url"], URL);
        let fast = service::fast_with_callbacks(
            "https://cloud.189.cn/t/AbC123",
            "",
            "f",
            "",
            |_| {
                let info = info.clone();
                async move { Ok::<_, ()>(Some(info)) }
            },
            |_, _| async { Ok::<_, ()>(Value::Null) },
            |m, _, _| async move {
                assert_eq!(m, DownloadMethod::AppShared);
                Ok::<_, ()>(Some(URL.into()))
            },
        )
        .await;
        assert_eq!(fast.unwrap(), URL);
    }
}

#[tokio::test]
async fn password_gated_info_is_refetched_before_parse_fast_and_file_metadata() {
    for route in ["parse", "fast", "file"] {
        let trace = RefCell::new(vec![]);
        let lookups = Cell::new(0);
        let get = |code: String| {
            trace.borrow_mut().push(json!(["share", code]));
            let first = lookups.replace(lookups.get() + 1) == 0;
            async move {
                Ok::<_, ()>(if first {
                    None
                } else {
                    Some(
                        json!({"fileId":"f","fileName":"Verified","fileSize":0,"isFolder":false,"shareId":"stale"}),
                    )
                })
            }
        };
        let check = |code: String, pwd: String| {
            trace.borrow_mut().push(json!(["pwd", code, pwd]));
            async { Ok::<_, ()>(json!({"success":true,"shareId":"verified"})) }
        };
        let download = |m, id: String, sid: String| {
            trace.borrow_mut().push(json!([method(m), id, sid]));
            assert_eq!(sid, "verified");
            async { Ok::<_, ()>(Some("https://offline.invalid/verified".into())) }
        };
        let list = |_, _, _| async {
            panic!("single file metadata is authoritative");
            #[allow(unreachable_code)]
            Ok::<_, ()>(vec![])
        };
        if route == "fast" {
            assert_eq!(
                service::fast_with_callbacks(
                    "https://cloud.189.cn/t/AbC123",
                    "pwd",
                    "f",
                    "",
                    get,
                    check,
                    download
                )
                .await
                .unwrap(),
                "https://offline.invalid/verified"
            );
        } else {
            let result = if route == "parse" {
                service::parse_with_callbacks(
                    "https://cloud.189.cn/t/AbC123",
                    "pwd",
                    &[],
                    "",
                    (get, check, list, download),
                )
                .await
            } else {
                service::file_action_with_metadata_callbacks(
                    "stale",
                    "f",
                    "pwd",
                    "AbC123",
                    (get, check, list, download),
                )
                .await
            };
            assert_eq!(result["code"], 200);
        }
        assert_eq!(
            *trace.borrow(),
            vec![
                json!(["share", "AbC123"]),
                json!(["pwd", "AbC123", "pwd"]),
                json!(["share", "AbC123"]),
                json!([
                    if route == "fast" { "app_dt1" } else { "open" },
                    "f",
                    "verified"
                ])
            ]
        );
    }
}

#[tokio::test]
async fn rejected_passwords_stop_every_route_without_listing_or_downloading() {
    for checked in [
        json!({"success":false,"shareId":"bait"}),
        json!({"success":"false","shareId":"bait"}),
        json!({}),
    ] {
        for route in ["parse", "fast", "file", "folder"] {
            let get = |_| async { Ok::<_, ()>(None) };
            let check = |_, _| {
                let checked = checked.clone();
                async move { Ok::<_, ()>(checked) }
            };
            let list = |_, _, _| async {
                panic!("rejected password must not list");
                #[allow(unreachable_code)]
                Ok::<_, ()>(vec![])
            };
            let download = |_, _, _| async {
                panic!("rejected password must not download");
                #[allow(unreachable_code)]
                Ok::<_, ()>(None)
            };
            let result = match route {
                "parse" => {
                    service::parse_with_callbacks(
                        "https://cloud.189.cn/t/AbC123",
                        "wrong",
                        &[],
                        "",
                        (get, check, list, download),
                    )
                    .await
                }
                "fast" => service::fast_with_callbacks(
                    "https://cloud.189.cn/t/AbC123",
                    "wrong",
                    "f",
                    "",
                    get,
                    check,
                    download,
                )
                .await
                .unwrap_err(),
                "file" => {
                    service::file_action_with_callbacks(
                        "s", "f", "", "0", "wrong", "AbC123", check, download,
                    )
                    .await
                }
                "folder" => {
                    service::folder_action_with_download_callbacks(
                        "s", "r", "f", "wrong", "1", "AbC123", check, list, download,
                    )
                    .await
                }
                _ => unreachable!(),
            };
            assert_eq!(result, password_error(), "{route}");
        }
    }
}

#[tokio::test]
async fn folder_item_resolves_real_files_directories_and_only_the_requested_subtree() {
    const URL: &str = "https://offline.invalid/file?signature=a%2Fb%2B%3D&x=1#part";
    for (target, code) in [
        ("sub", 200),
        ("r", 200),
        ("video", 200),
        ("zero", 200),
        ("outside", 404),
    ] {
        let lists = RefCell::new(vec![]);
        let downloads = RefCell::new(vec![]);
        let result = service::folder_action_with_download_callbacks(
            "s", "r", target, "", "2", "",
            |_, _| async { Ok::<_, ()>(Value::Null) },
            |sid: String, id: String, mode: String| {
                assert_eq!((sid.as_str(),mode.as_str()), ("s","2"));
                lists.borrow_mut().push(id.clone());
                async move { Ok::<_, ()>(match id.as_str() {
                    "r" => vec![json!({"fileId":"sub","fileName":"真实目录","isFolder":true})],
                    "sub" => vec![json!({"fileId":"video","fileName":"真实名字.mkv","fileSize":"9007199254740993","isFolder":false}),json!({"fileId":"zero","fileName":"zero.txt","fileSize":0,"isFolder":false,"isDir":true})],
                    _ => panic!("must never search outside subtree"),
                }) }
            },
            |m, id: String, sid: String| {
                downloads.borrow_mut().push(json!([method(m),id,sid]));
                assert_eq!(sid,"s");
                async move { Ok::<_, ()>(if m == DownloadMethod::AppShared {Some(URL.into())} else {None}) }
            },
        ).await;
        assert_eq!(result["code"], code, "{target}");
        assert_eq!(*lists.borrow(), ["r", "sub"]);
        if matches!(target, "video" | "zero") {
            assert_eq!(result["data"]["fileId"], target);
            assert_eq!(
                result["data"]["fileName"],
                if target == "zero" {
                    "zero.txt"
                } else {
                    "真实名字.mkv"
                }
            );
            assert_eq!(
                result["data"]["fileSize"],
                if target == "zero" {
                    json!(0)
                } else {
                    json!("9007199254740993")
                }
            );
            assert_eq!(
                result["data"]["downloadUrl"], URL,
                "signed URL stays byte-identical"
            );
            assert_eq!(
                *downloads.borrow(),
                vec![
                    json!(["open", target, "s"]),
                    json!(["app_dt1", target, "s"])
                ]
            );
        } else {
            assert!(downloads.borrow().is_empty());
            if target == "sub" {
                assert_eq!(
                    result["data"]["tree"],
                    json!({"真实目录":{"真实名字.mkv":{"id":"video","size":"9007199254740993"},"zero.txt":{"id":"zero","size":0}}})
                );
            }
        }
    }
    let result = service::folder_action_with_callbacks(
        "s",
        "r",
        "f",
        "",
        "1",
        "",
        |_, _| async { Ok::<_, ()>(Value::Null) },
        |_, _, _| async {
            Ok::<_, ()>(vec![json!({"fileId":"f","fileName":"known","fileSize":0})])
        },
    )
    .await;
    assert_eq!(
        result,
        public_file_download_failure("f", "known", json!(0)),
        "list-only seam never fabricates a successful file download"
    );
}

#[tokio::test]
async fn incomplete_invalid_cyclic_and_resource_limited_trees_never_return_success() {
    for scenario in [
        "root-error",
        "child-error",
        "cycle",
        "duplicate",
        "invalid-size",
        "limit",
        "depth",
    ] {
        for route in ["parse", "folder", "file"] {
            let root = json!({"fileId":"r","fileName":"Root","isFolder":true,"shareId":"s"});
            let get = |_| {
                let root = root.clone();
                async move { Ok::<_, &str>(Some(root)) }
            };
            let check = |_, _| async { Ok::<_, &str>(Value::Null) };
            let list = |_: String, id: String, _: String| async move {
                if scenario == "root-error" || (scenario == "child-error" && id == "a") {
                    return Err("synthetic-upstream-secret");
                }
                Ok(match scenario {
                    "child-error" if id == "r" => vec![
                        json!({"fileId":"f","fileName":"found-before-failure","fileSize":0}),
                        json!({"fileId":"a","fileName":"A","isFolder":true}),
                    ],
                    "cycle" => vec![json!({"fileId":"r","fileName":"cycle","isFolder":true})],
                    "duplicate" => vec![
                        json!({"fileId":"a","fileName":"A","isFolder":true}),
                        json!({"fileId":"a","fileName":"again","isFolder":true}),
                    ],
                    "invalid-size" => {
                        vec![json!({"fileId":"f","fileName":"negative","fileSize":-1})]
                    }
                    "limit" => (0..=tree::MAX_PRODUCTION_ENTRIES)
                        .map(|i| json!({"fileId":format!("f{i}"),"fileName":"f","fileSize":0}))
                        .collect(),
                    "depth" => {
                        let n = if id == "r" {
                            0
                        } else {
                            id[1..].parse::<usize>().unwrap()
                        };
                        vec![json!({"fileId":format!("d{}",n+1),"fileName":"d","isFolder":true})]
                    }
                    _ => vec![],
                })
            };
            let download = |_, _, _| async {
                panic!("incomplete trees cannot download a discovered file");
                #[allow(unreachable_code)]
                Ok::<_, &str>(None)
            };
            let result = if route == "parse" {
                let (result, index) = service::parse_indexed_with_callbacks(
                    "https://cloud.189.cn/t/AbC123",
                    "",
                    &[],
                    "f",
                    (get, check, list, download),
                )
                .await;
                assert_eq!(index, None, "no partial directory index {scenario}");
                result
            } else if route == "folder" {
                service::folder_action_with_download_callbacks(
                    "s", "r", "f", "", "1", "", check, list, download,
                )
                .await
            } else {
                service::file_action_with_metadata_callbacks(
                    "s",
                    "f",
                    "",
                    "AbC123",
                    (get, check, list, download),
                )
                .await
            };
            assert_eq!(result["success"], false, "{route}/{scenario}");
            assert_eq!(result["data"], Value::Null);
            assert_eq!(
                result["code"],
                if matches!(scenario, "limit" | "depth") {
                    413
                } else {
                    502
                }
            );
            assert!(!result.to_string().contains("synthetic-upstream-secret"));
        }
    }
}

#[tokio::test]
async fn decimal_callback_sizes_are_exact_or_unknown_and_signed_urls_are_unchanged() {
    const URL: &str = "https://offline.invalid/file?signature=x%2B%3D#frag";
    for (input, expected) in [
        ("0", json!(0)),
        (" 42 ", json!(42)),
        ("9007199254740991", json!(9007199254740991u64)),
        ("9007199254740993", json!("9007199254740993")),
        ("18446744073709551615", json!("18446744073709551615")),
        ("18446744073709551616", Value::Null),
        ("", Value::Null),
        ("unknown", Value::Null),
        ("-1", Value::Null),
        ("1.5", Value::Null),
        ("12suffix", Value::Null),
        ("0x20", Value::Null),
        ("1e3", Value::Null),
    ] {
        let result = service::file_action_with_callbacks(
            "s",
            "f",
            "中 文.mp4",
            input,
            "",
            "",
            |_, _| async { Ok::<_, ()>(Value::Null) },
            |_, _, _| async { Ok::<_, ()>(Some(URL.into())) },
        )
        .await;
        assert_eq!(result["data"]["fileSize"], expected, "{input}");
        assert_eq!(result["data"]["downloadUrl"], URL);
    }
}

#[tokio::test]
async fn metadata_callbacks_lookup_share_id_roots_and_reject_missing_or_directory_ids() {
    for code in ["", "AbC123"] {
        for (target, status) in [("f", 200), ("a", 400), ("missing", 404)] {
            let requests = RefCell::new(vec![]);
            let result = service::file_action_with_metadata_callbacks("s", target, "", code, (
                |_| async { Ok::<_, ()>(Some(json!({"fileId":"r","fileName":"R","shareId":"s","isFolder":true}))) },
                |_, _| async { Ok::<_, ()>(Value::Null) },
                |sid: String, id: String, mode: String| {
                    assert_eq!((sid.as_str(),mode.as_str()),("s","1"));
                    requests.borrow_mut().push(id.clone());
                    async move { Ok::<_, ()>(if id == "a" {vec![json!({"fileId":"f","fileName":"upstream","fileSize":0,"size":123})]} else {vec![json!({"fileId":"a","fileName":"A","isFolder":true})]}) }
                },
                |m, id, sid| async move { assert_eq!((m,id.as_str(),sid.as_str()), (DownloadMethod::Open,"f","s")); Ok::<_, ()>(Some("https://offline.invalid/upstream".into())) },
            )).await;
            assert_eq!(result["code"], status, "{code}/{target}");
            assert_eq!(
                *requests.borrow(),
                [if code.is_empty() { "" } else { "r" }, "a"]
            );
            if target == "f" {
                assert_eq!(result["data"]["fileName"], "upstream");
                assert_eq!(result["data"]["fileSize"], 0);
            }
        }
    }
}

#[tokio::test]
async fn production_parse_file_ignores_forged_client_metadata_and_preserves_size_precision() {
    const URL: &str = "https://offline.invalid/file?signature=a%2Fb%2B%3D&x=1";
    for route in ["single", "folder", "share-id"] {
        let info = if route == "single" {
            json!({"res_code":0,"fileId":"f","fileName":"真实文件.txt","fileSize":9007199254740993u64,"isFolder":0,"shareId":"s"})
        } else {
            json!({"res_code":0,"fileId":"r","fileName":"R","isFolder":1,"shareId":"s"})
        };
        let mut responses = vec![];
        if route != "share-id" {
            responses.push((info, vec![]));
        }
        if route != "single" {
            responses.push((json!({"res_code":0,"fileList":[{"id":"f","name":"真实文件.txt","size":9007199254740993u64}]}),vec![]));
        }
        let prefix = responses.len();
        responses.push((json!({"fileDownloadUrl":URL}), vec![]));
        let (base, handle) = stub(responses);
        let client = loopback_client(
            &base,
            json!({"open_access_token":"offline-open-token-123456789"}),
        );
        let result = service::parse_file(
            client,
            "s",
            "f",
            "伪造名字.mp4",
            "1",
            "",
            if route == "share-id" { "" } else { "AbC123" },
        )
        .await;
        assert_eq!(result["code"], 200, "{route}");
        assert_eq!(result["data"]["fileName"], "真实文件.txt");
        assert_eq!(result["data"]["fileSize"], "9007199254740993");
        assert_eq!(result["data"]["downloadUrl"], URL);
        let requests = handle.join().unwrap();
        assert_public_download_requests(&requests[prefix..], "f", "s", &["open"]);
    }
}

#[tokio::test]
async fn production_folder_item_uses_captured_password_cookie_and_real_file_metadata() {
    const URL: &str = "https://offline.invalid/file?signature=a%2Fb%3D&amp;x=1&amp;y=2";
    let (base, handle) = stub(vec![
        (
            json!({"res_code":0,"shareId":"verified"}),
            vec!["SHARE_SESSION=synthetic; Path=/"],
        ),
        (
            json!({"res_code":0,"folderList":[{"id":"a","name":"A"}]}),
            vec![],
        ),
        (
            json!({"res_code":0,"fileList":[{"id":"f","name":"真实文件","size":0}]}),
            vec![],
        ),
        (json!({"fileDownloadUrl":URL}), vec![]),
    ]);
    let client = loopback_client(
        &base,
        json!({"open_access_token":"offline-open-token-123456789"}),
    );
    let result = service::browse_folder_item(client, "stale", "r", "f", "pwd", "2", "AbC123").await;
    assert_eq!(result["data"]["fileName"], "真实文件");
    assert_eq!(result["data"]["fileSize"], 0);
    assert_eq!(
        result["data"]["downloadUrl"],
        "https://offline.invalid/file?signature=a%2Fb%3D&x=1&y=2"
    );
    let requests = handle.join().unwrap();
    assert_eq!(requests.len(), 4);
    for req in &requests[1..3] {
        assert_eq!(req["headers"]["cookie"], "SHARE_SESSION=synthetic");
        assert!(req["target"].as_str().unwrap().contains("shareId=verified"));
    }
    assert_eq!(requests[3]["headers"]["cookie"], "SHARE_SESSION=synthetic");
    assert_public_download_requests(&requests[3..], "f", "verified", &["open"]);
}

#[tokio::test]
async fn production_rejected_passwords_are_403_on_all_routes() {
    for route in ["parse", "fast", "file", "folder"] {
        let mut responses = vec![];
        if route != "folder" {
            responses.push((
                json!({"res_code":0,"fileId":"r","fileName":"R","isFolder":1,"shareId":"s"}),
                vec![],
            ));
        }
        responses.push((json!({"res_code":403,"shareId":"bait"}), vec![]));
        let expected_count = responses.len();
        let (base, handle) = stub(responses);
        let client = loopback_client(
            &base,
            json!({"session_key":"offline-session","session_secret":"offline-secret","open_access_token":"offline-open-token-123456789"}),
        );
        let result = match route {
            "parse" => {
                service::parse(client, "https://cloud.189.cn/t/AbC123", "wrong", &[], "").await
            }
            "fast" => {
                service::fast_download(client, "https://cloud.189.cn/t/AbC123", "wrong", "f", "")
                    .await
                    .unwrap_err()
            }
            "file" => service::parse_file(client, "s", "f", "bait", "0", "wrong", "AbC123").await,
            "folder" => {
                service::browse_folder_item(client, "s", "r", "f", "wrong", "1", "AbC123").await
            }
            _ => unreachable!(),
        };
        assert_eq!(result, password_error(), "{route}");
        assert_eq!(
            handle.join().unwrap().len(),
            expected_count,
            "no request after password rejection"
        );
    }
}

#[tokio::test]
async fn production_password_refetch_keeps_cookies_for_the_complete_tree() {
    let (base, handle) = stub(vec![
        (json!({"res_code":403}), vec![]),
        (
            json!({"res_code":0,"shareId":"s"}),
            vec!["SHARE_SESSION=synthetic; Path=/"],
        ),
        (
            json!({"res_code":0,"fileId":"r","fileName":"R","isFolder":1,"shareId":"s"}),
            vec![],
        ),
        (json!({"res_code":0,"fileList":[]}), vec![]),
    ]);
    let result = service::parse(
        loopback_client(&base, json!({})),
        "https://cloud.189.cn/t/AbC123",
        "pwd",
        &[],
        "",
    )
    .await;
    assert_eq!(result["code"], 200);
    assert_eq!(result["data"]["tree"], json!({"R":{}}));
    let requests = handle.join().unwrap();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[3]["headers"]["cookie"], "SHARE_SESSION=synthetic");
}

#[tokio::test]
async fn production_pagination_and_more_than_400_directories_are_complete() {
    let mut responses = vec![(
        json!({"res_code":0,"fileId":"r","fileName":"Root","isFolder":1,"shareId":"s"}),
        vec![],
    )];
    for range in [0..200, 200..400, 400..450] {
        responses.push((json!({"res_code":0,"count":450,"folderList":range.map(|i| json!({"id":format!("d{i}"),"name":format!("D{i}")})).collect::<Vec<_>>()}),vec![]));
    }
    responses.extend((0..450).map(|_| (json!({"res_code":0,"fileList":[]}), vec![])));
    let (base, handle) = stub(responses);
    let client = loopback_client(&base, json!({})).with_limit(settings::UpstreamLimit::new(2));
    assert_eq!(client.concurrency(), 2);
    let (result, index) =
        service::parse_indexed(client, "https://cloud.189.cn/t/AbC123", "", &[], "").await;
    assert_eq!(result["success"], true);
    assert_eq!(
        result["data"]["tree"]["Root"].as_object().unwrap().len(),
        450
    );
    for i in 0..450 {
        assert_eq!(result["data"]["tree"]["Root"][format!("D{i}")], json!({}));
    }
    assert_eq!(index.unwrap().1.len(), 451);
    let requests = handle.join().unwrap();
    assert_eq!(requests.len(), 454);
    for (i, request) in requests[1..4].iter().enumerate() {
        assert!(
            request["target"]
                .as_str()
                .unwrap()
                .contains(&format!("pageNum={}", i + 1))
        );
    }
}

#[tokio::test]
async fn production_late_page_failure_is_not_partial_success_or_a_directory_index() {
    let (base, handle) = stub(vec![
        (
            json!({"res_code":0,"fileId":"r","fileName":"Root","isFolder":1,"shareId":"s"}),
            vec![],
        ),
        (
            json!({"res_code":0,"count":2,"fileList":[{"id":"f","name":"already-found","size":0}]}),
            vec![],
        ),
        (json!({"res_code":403,"fileList":[]}), vec![]),
    ]);
    let (result, index) = service::parse_indexed(
        loopback_client(&base, json!({})),
        "https://cloud.189.cn/t/AbC123",
        "",
        &[],
        "f",
    )
    .await;
    assert_eq!(result, public_error(502, "天翼目录列表获取失败"));
    assert_eq!(index, None);
    assert_eq!(
        handle.join().unwrap().len(),
        3,
        "no fallback that mixes partial pages"
    );
}

// Synthetic URL reproduces the encoded annotation without containing a real share.
const PASSWORD_SHARE: &str = "https://cloud.189.cn/web/share?code=AbC123%EF%BC%88%E8%AE%BF%E9%97%AE%E7%A0%81%EF%BC%9AaB12%EF%BC%89";

#[tokio::test]
async fn listed_password_share_reverifies_on_fresh_fast_client_even_with_sid() {
    for app_succeeds in [true, false] {
        let info =
            json!({"res_code":0,"fileId":"r","fileName":"Root","isFolder":1,"shareId":"stale"});
        let direct = "https://offline.invalid/signed?x=a%2Bb%3D&y=2";
        let mut responses = vec![
            (info.clone(), vec![]),
            (
                json!({"res_code":0,"shareId":"listed"}),
                vec!["SHARE_SESSION=listing; Path=/"],
            ),
            (
                json!({"res_code":0,"fileList":[{"id":"f","name":"movie","size":0}]}),
                vec![],
            ),
            (info, vec![]),
            (
                json!({"res_code":0,"shareId":"verified"}),
                vec!["SHARE_SESSION=fresh-download; Path=/"],
            ),
            (
                if app_succeeds {
                    json!({"fileDownloadUrl":direct})
                } else {
                    json!({"res_code":403})
                },
                vec![],
            ),
        ];
        if !app_succeeds {
            responses.push((json!({"fileDownloadUrl":direct}), vec![]));
        }
        let (base, handle) = stub(responses);
        let credentials = json!({"session_key":"offline-session","session_secret":"offline-secret","open_access_token":"offline-open"});
        let listed = service::parse(
            loopback_client(&base, credentials.clone()),
            PASSWORD_SHARE,
            "aB12",
            &[],
            "",
        )
        .await;
        assert_eq!(listed["code"], 200);
        assert_eq!(listed["data"]["sid"], "listed");
        let result = service::fast_download(
            loopback_client(&base, credentials),
            PASSWORD_SHARE,
            "aB12",
            "f",
            "listed",
        )
        .await;
        assert_eq!(result.unwrap(), direct);
        let requests = handle.join().unwrap();
        for index in [0, 3] {
            let url = url::Url::parse(&format!(
                "http://offline.invalid{}",
                requests[index]["target"].as_str().unwrap()
            ))
            .unwrap();
            assert_eq!(
                url.query_pairs()
                    .find(|(key, _)| key == "shareCode")
                    .unwrap()
                    .1,
                "AbC123"
            );
            assert!(
                requests[index]["headers"]["cookie"].is_null(),
                "fresh clients must not inherit another request's cookies"
            );
        }
        for index in [1, 4] {
            assert_eq!(requests[index]["method"], "POST");
            assert_eq!(
                requests[index]["body"],
                "shareCode=AbC123&accessCode=aB12&validateCode="
            );
        }
        assert_eq!(requests[2]["headers"]["cookie"], "SHARE_SESSION=listing");
        let methods: &[&str] = if app_succeeds {
            &["app_dt1"]
        } else {
            &["app_dt1", "open"]
        };
        assert_public_download_requests(&requests[5..], "f", "verified", methods);
        for request in &requests[5..] {
            assert_eq!(request["headers"]["cookie"], "SHARE_SESSION=fresh-download");
        }
    }
}

#[tokio::test]
async fn fast_sid_does_not_override_wrong_password_or_invent_missing_password() {
    for pwd in ["", "wrong"] {
        let responses = if pwd.is_empty() {
            vec![(json!({"res_code":403}), vec![]); 2]
        } else {
            vec![
                (
                    json!({"res_code":0,"fileId":"r","isFolder":1,"shareId":"s"}),
                    vec![],
                ),
                (json!({"res_code":403,"shareId":"bait"}), vec![]),
            ]
        };
        let (base, handle) = stub(responses);
        let result = service::fast_download(
            loopback_client(&base, json!({"session_key":"offline-session","session_secret":"offline-secret","open_access_token":"offline-open"})),
            PASSWORD_SHARE, pwd, "f", "s",
        ).await;
        let requests = handle.join().unwrap();
        if pwd.is_empty() {
            assert_eq!(result.unwrap_err(), public_fast_download_failure());
            assert_public_download_requests(&requests, "f", "s", &["app_dt1", "open"]);
            assert!(
                requests
                    .iter()
                    .all(|request| request["headers"]["cookie"].is_null())
            );
        } else {
            assert_eq!(result.unwrap_err(), password_error());
            assert_eq!(
                requests.len(),
                2,
                "no download or owned fallback after password rejection"
            );
            assert!(
                requests[1]["target"]
                    .as_str()
                    .unwrap()
                    .starts_with("/open/share/checkAccessCode.action")
            );
            assert_eq!(
                requests[1]["body"],
                "shareCode=AbC123&accessCode=wrong&validateCode="
            );
        }
    }
    // A supplied password with no reconstructible share code must fail, not be ignored.
    let result = service::fast_download(
        adapter::Cloud189Client::new(reqwest::Client::new(), json!({})),
        "invalid",
        "aB12",
        "f",
        "s",
    )
    .await;
    assert_eq!(result.unwrap_err()["code"], 502);
}

#[tokio::test]
async fn cancelled_password_verification_never_advances_to_shared_downloads() {
    let checks = Cell::new(0);
    let downloads = Cell::new(0);
    let mut future = Box::pin(service::fast_with_callbacks(
        PASSWORD_SHARE,
        "aB12",
        "f",
        "listed",
        |_| async {
            Ok::<_, ()>(Some(
                json!({"fileId":"r","shareId":"listed","isFolder":true}),
            ))
        },
        |code, pwd| {
            assert_eq!((code.as_str(), pwd.as_str()), ("AbC123", "aB12"));
            checks.set(checks.get() + 1);
            std::future::pending::<Result<Value, ()>>()
        },
        |_, _, _| {
            downloads.set(downloads.get() + 1);
            async { Ok::<_, ()>(Some("https://offline.invalid/bait".into())) }
        },
    ));
    assert!(futures_util::poll!(&mut future).is_pending());
    assert_eq!(checks.get(), 1, "sid must not bypass a supplied password");
    drop(future);
    assert_eq!(downloads.get(), 0);
}

#[tokio::test]
async fn public_mode_two_ignores_extraneous_pansou_password_but_protected_shares_still_check() {
    for protected in [false, true] {
        let meta = if protected {
            json!({"res_code":0,"fileId":"root","isFolder":true,"shareMode":1,"needAccessCode":1})
        } else {
            json!({"res_code":0,"fileId":"root","isFolder":true,"shareId":"share","shareMode":2,"accessCode":""})
        };
        let next = if protected {
            json!({"res_code":0,"success":false})
        } else {
            json!({"res_code":0,"fileListAO":{"count":1,"folderList":[],"fileList":[{"id":"file","name":"file.dat","size":0}]}})
        };
        let (base, handle) = stub(vec![(meta, vec![]), (next, vec![])]);
        let result = service::parse(
            loopback_client(&base, json!({})),
            "https://cloud.189.cn/t/AbC123",
            "extraneous-code",
            &[],
            "",
        )
        .await;
        assert_eq!(result["code"], if protected { 403 } else { 200 });
        let requests = handle.join().unwrap();
        assert_eq!(requests.len(), 2);
        let path = requests[1]["target"].as_str().unwrap();
        assert_eq!(path.contains("checkAccessCode"), protected);
        if !protected {
            assert!(path.contains("shareMode=2"));
        }
    }
}

#[tokio::test]
async fn production_download_auth_rejection_is_401_unless_another_shared_method_succeeds() {
    const URL: &str = "https://offline.invalid/shared?signature=a%2Bb%3D";
    for route in ["single", "file", "fast"] {
        for fallback_succeeds in [false, true] {
            let mut responses = match route {
                "single" => vec![(
                    json!({"res_code":0,"fileId":"f","fileName":"Film","fileSize":0,"isFolder":false,"shareId":"s"}),
                    vec![],
                )],
                "file" => vec![(
                    json!({"res_code":0,"fileList":[{"id":"f","name":"Film","size":0}]}),
                    vec![],
                )],
                _ => vec![],
            };
            let prefix = responses.len();
            let rejection = json!({"errorCode":"InvalidSessionKey","errorMsg":"synthetic-secret-upstream","fileDownloadUrl":"https://offline.invalid/forbidden-bait"});
            responses.push((rejection.clone(), vec![]));
            responses.push((
                if fallback_succeeds {
                    json!({"fileDownloadUrl":URL})
                } else {
                    rejection
                },
                vec![],
            ));
            let (base, handle) = stub(responses);
            let client = loopback_client(
                &base,
                json!({"session_key":"offline-session","session_secret":"offline-secret","open_access_token":"offline-open-token"}),
            );
            let result = match route {
                "single" => {
                    service::parse(client, "https://cloud.189.cn/t/AbC123", "", &[], "").await
                }
                "file" => service::parse_file(client, "s", "f", "", "", "", "").await,
                "fast" => match service::fast_download(client, "invalid", "", "f", "s").await {
                    Ok(url) => json!({"code":200,"url":url}),
                    Err(e) => e,
                },
                _ => unreachable!(),
            };
            let requests = handle.join().unwrap();
            assert_public_download_requests(
                &requests[prefix..],
                "f",
                "s",
                if route == "fast" {
                    &["app_dt1", "open"]
                } else {
                    &["open", "app_dt1"]
                },
            );
            if fallback_succeeds {
                assert_eq!(result["code"], 200, "{route}");
                assert!(result.to_string().contains(URL));
            } else {
                assert_eq!(result, auth_required_error(), "{route}");
            }
            for secret in [
                "synthetic-secret",
                "offline-session",
                "offline-secret",
                "offline-open-token",
                "forbidden-bait",
                "InvalidSessionKey",
            ] {
                assert!(!result.to_string().contains(secret), "sanitized {route}");
            }
        }
    }
}

fn classified_failure(kind: &'static str) -> adapter::Cloud189Error {
    adapter::Cloud189Error {
        operation: "synthetic-secret-operation",
        kind,
    }
}
fn auth_required_error() -> Value {
    json!({"code":401,"msg":"天翼下载认证被拒绝，请检查后台登录状态",
        "success":false,"data":null,"error_code":"cloud189_auth_required","need_relogin":true})
}

// Run the real orchestration across every download entry point with typed failures.
async fn classified_download_response(
    route: &str,
    first: Result<Option<String>, adapter::Cloud189Error>,
    second: Result<Option<String>, adapter::Cloud189Error>,
) -> (Value, Vec<&'static str>) {
    let calls = RefCell::new(vec![]);
    let download = |m: DownloadMethod, id: String, sid: String| {
        assert_eq!((id.as_str(), sid.as_str()), ("f", "s"));
        calls.borrow_mut().push(method(m));
        let result = if calls.borrow().len() == 1 {
            first.clone()
        } else {
            second.clone()
        };
        async move { result }
    };
    let get = |_| async {
        Ok::<_, adapter::Cloud189Error>(Some(
            json!({"fileId":"f","fileName":"Film","fileSize":0,"isFolder":false,"shareId":"s"}),
        ))
    };
    let check = |_, _| async { Ok::<_, adapter::Cloud189Error>(Value::Null) };
    let list = |_, _, _| async {
        Ok::<_, adapter::Cloud189Error>(vec![
            json!({"fileId":"f","fileName":"Film","fileSize":0,"isFolder":false}),
        ])
    };
    let value = match route {
        "single" => {
            service::parse_with_callbacks(
                "https://cloud.189.cn/t/AbC123",
                "",
                &[],
                "",
                (get, check, list, download),
            )
            .await
        }
        "targeted" => {
            service::parse_with_callbacks(
                "https://cloud.189.cn/t/AbC123",
                "",
                &[],
                "f",
                (
                    |_| async {
                        Ok::<_, adapter::Cloud189Error>(Some(
                            json!({"fileId":"r","fileName":"Root","isFolder":true,"shareId":"s"}),
                        ))
                    },
                    check,
                    list,
                    download,
                ),
            )
            .await
        }
        "file" => {
            service::file_action_with_callbacks("s", "f", "Film", "0", "", "", check, download)
                .await
        }
        "metadata" => {
            service::file_action_with_metadata_callbacks(
                "s",
                "f",
                "",
                "AbC123",
                (get, check, list, download),
            )
            .await
        }
        "folder" => {
            service::folder_action_with_download_callbacks(
                "s", "r", "f", "", "1", "", check, list, download,
            )
            .await
        }
        "fast" => match service::fast_with_callbacks("invalid", "", "f", "s", get, check, download)
            .await
        {
            Ok(url) => json!({"code":200,"url":url}),
            Err(e) => e,
        },
        _ => unreachable!(),
    };
    (value, calls.into_inner())
}

#[tokio::test]
async fn download_auth_rejection_is_preserved_but_public_alternatives_can_succeed() {
    const URL: &str = "https://offline.invalid/shared?signature=a%2Bb%3D";
    for route in ["single", "targeted", "file", "metadata", "folder", "fast"] {
        let methods = if route == "fast" {
            vec!["app_dt1", "open"]
        } else {
            vec!["open", "app_dt1"]
        };
        for other in [
            Ok(None),
            Ok(Some(String::new())),
            Err(classified_failure("synthetic-secret-kind")),
            Err(classified_failure("share unavailable")),
        ] {
            for auth_first in [true, false] {
                let auth = Err(classified_failure("authentication required"));
                let (first, second) = if auth_first {
                    (auth, other.clone())
                } else {
                    (other.clone(), auth)
                };
                let (value, calls) = classified_download_response(route, first, second).await;
                assert_eq!(value, auth_required_error(), "{route}/{auth_first}");
                assert_eq!(calls, methods);
                assert!(!value.to_string().contains("synthetic-secret"));
                assert!(!value["msg"].as_str().unwrap().contains("全部"));
            }
        }
        let (value, calls) = classified_download_response(
            route,
            Err(classified_failure("authentication required")),
            Ok(Some(URL.into())),
        )
        .await;
        assert_eq!(
            value["code"], 200,
            "{route}: the other shared method is legal"
        );
        assert!(value.to_string().contains(URL));
        assert_eq!(calls, methods);
        let (value, calls) = classified_download_response(
            route,
            Ok(Some(URL.into())),
            Err(classified_failure("authentication required")),
        )
        .await;
        assert_eq!(value["code"], 200);
        assert_eq!(calls, &methods[..1], "stop at first valid shared URL");
    }
}

#[tokio::test]
async fn useful_download_errors_are_sanitized_and_none_keeps_legacy_responses() {
    for route in ["single", "targeted", "file", "metadata", "folder", "fast"] {
        for (kind, code, msg) in [
            ("share unavailable", 404, "天翼分享不可用，请检查分享链接"),
            (
                "access code required",
                403,
                "天翼分享需要提取码，请填写提取码",
            ),
            (
                "access code rejected",
                403,
                "提取码错误或已过期，请检查提取码是否正确",
            ),
            ("response limit exceeded", 413, "天翼请求超过资源上限"),
            ("synthetic-secret-kind", 502, "天翼下载链接请求失败"),
        ] {
            for useful_first in [true, false] {
                let useful = Err(classified_failure(kind));
                let opaque = Err(classified_failure("synthetic-secret-kind"));
                let (first, second) = if useful_first {
                    (useful, opaque)
                } else {
                    (opaque, useful)
                };
                let (value, _) = classified_download_response(route, first, second).await;
                assert_eq!(
                    value,
                    public_error(code, msg),
                    "{route}/{kind}/{useful_first}"
                );
                assert!(!value.to_string().contains("synthetic-secret"));
                assert!(value.get("need_relogin").is_none());
            }
        }
        let (value, _) =
            classified_download_response(route, Ok(None), Ok(Some(String::new()))).await;
        let expected = match route {
            "fast" => public_fast_download_failure(),
            "file" | "metadata" | "folder" => public_file_download_failure("f", "Film", json!(0)),
            _ => public_parse_download_failure(),
        };
        assert_eq!(value, expected, "{route}: no-URL compatibility");
    }
}

#[tokio::test]
async fn prepare_and_password_checks_preserve_classified_errors_without_secrets() {
    for route in ["single", "metadata", "fast", "file", "folder"] {
        for stage in ["share", "check", "refetch"] {
            if matches!(route, "file" | "folder") && stage != "check" {
                continue;
            }
            for (kind, expected) in [
                ("authentication required", auth_required_error()),
                (
                    "share unavailable",
                    public_error(404, "天翼分享不可用，请检查分享链接"),
                ),
                (
                    "access code required",
                    public_error(403, "天翼分享需要提取码，请填写提取码"),
                ),
                ("access code rejected", password_error()),
            ] {
                let lookups = Cell::new(0);
                let get = |_| {
                    let count = lookups.replace(lookups.get() + 1);
                    async move {
                        if stage == "share" || (stage == "refetch" && count > 0) {
                            Err(classified_failure(kind))
                        } else {
                            Ok(None)
                        }
                    }
                };
                let check = |_, _| async move {
                    if stage == "check" {
                        Err(classified_failure(kind))
                    } else {
                        Ok(json!({"success":true,"shareId":"s"}))
                    }
                };
                let list = |_, _, _| async {
                    panic!("preparation failure must not list");
                    #[allow(unreachable_code)]
                    Ok::<_, adapter::Cloud189Error>(vec![])
                };
                let download = |_, _, _| async {
                    panic!("preparation failure must not download");
                    #[allow(unreachable_code)]
                    Ok::<_, adapter::Cloud189Error>(None)
                };
                let value = match route {
                    "single" => {
                        service::parse_with_callbacks(
                            "https://cloud.189.cn/t/AbC123",
                            "synthetic-secret-password",
                            &[],
                            "",
                            (get, check, list, download),
                        )
                        .await
                    }
                    "metadata" => {
                        service::file_action_with_metadata_callbacks(
                            "s",
                            "f",
                            "synthetic-secret-password",
                            "AbC123",
                            (get, check, list, download),
                        )
                        .await
                    }
                    "fast" => service::fast_with_callbacks(
                        "https://cloud.189.cn/t/AbC123",
                        "synthetic-secret-password",
                        "f",
                        "s",
                        get,
                        check,
                        download,
                    )
                    .await
                    .unwrap_err(),
                    "file" => {
                        service::file_action_with_callbacks(
                            "s",
                            "f",
                            "Film",
                            "0",
                            "synthetic-secret-password",
                            "AbC123",
                            check,
                            download,
                        )
                        .await
                    }
                    "folder" => {
                        service::folder_action_with_download_callbacks(
                            "s",
                            "r",
                            "f",
                            "synthetic-secret-password",
                            "1",
                            "AbC123",
                            check,
                            list,
                            download,
                        )
                        .await
                    }
                    _ => unreachable!(),
                };
                assert_eq!(value, expected, "{route}/{stage}/{kind}");
                assert!(!value.to_string().contains("synthetic-secret"));
                assert_eq!(
                    lookups.get(),
                    if matches!(route, "file" | "folder") {
                        0
                    } else if stage == "refetch" {
                        2
                    } else {
                        1
                    }
                );
            }
        }
    }
}

// Independent synthetic regressions, not deployment evidence. All API traffic is
// loopback-only; the supplier-shaped signed URL below is returned, never fetched.
const SYNTHETIC_FILE_TRAMPOLINE: &str = "https://download.cloud.189.cn/file/downloadFile.action?fileId=synthetic-file&shareId=synthetic-share&signature=a%2Fb%2B%3D&x=1#part";

#[tokio::test]
async fn synthetic_file_trampoline_parse_and_fast_preserve_public_order_without_fetching() {
    for route in ["single-parse", "single-fast", "folder-file-fast"] {
        for reply in ["both", "open-only", "app-only", "neither"] {
            let fast = route != "single-parse";
            let folder = route == "folder-file-fast";
            let root_id = if folder {
                "synthetic-root"
            } else {
                "synthetic-file"
            };
            let mut responses = vec![(
                json!({"res_code":0,"fileId":root_id,"fileName":"Synthetic",
                    "fileSize":7,"isFolder":if folder { 1 } else { 0 },
                    "shareId":"synthetic-share"}),
                vec![],
            )];
            let mut methods = vec![];
            for key in if fast {
                ["app_dt1", "open"]
            } else {
                ["open", "app_dt1"]
            } {
                methods.push(key);
                let succeeds = reply == "both"
                    || (reply == "open-only" && key == "open")
                    || (reply == "app-only" && key == "app_dt1");
                responses.push((
                    json!({"fileDownloadUrl":if succeeds { SYNTHETIC_FILE_TRAMPOLINE } else { "" }}),
                    vec![],
                ));
                if succeeds {
                    break;
                }
            }
            let (base, handle) = stub(responses);
            let client = loopback_client(
                &base,
                json!({"session_key":"offline-session","session_secret":"offline-secret",
                    "open_access_token":"offline-open-token-123456789"}),
            );
            if fast {
                // Empty sid forces authoritative metadata lookup. A directory
                // share root must not cause its different file ID to be rejected.
                let result = service::fast_download(
                    client,
                    "https://cloud.189.cn/t/AbC123",
                    "",
                    "synthetic-file",
                    "",
                )
                .await;
                if reply == "neither" {
                    assert_eq!(
                        result.unwrap_err(),
                        public_fast_download_failure(),
                        "{route}/{reply}"
                    );
                } else {
                    assert_eq!(
                        result.unwrap(),
                        SYNTHETIC_FILE_TRAMPOLINE,
                        "{route}/{reply}"
                    );
                }
            } else {
                let result =
                    service::parse(client, "https://cloud.189.cn/t/AbC123", "", &[], "").await;
                if reply == "neither" {
                    assert_eq!(result, public_parse_download_failure(), "{route}/{reply}");
                } else {
                    assert_eq!(result["code"], 200, "{route}/{reply}");
                    assert_eq!(result["success"], true);
                    assert_eq!(result["data"]["is_folder"], false);
                    assert_eq!(result["data"]["download_url"], SYNTHETIC_FILE_TRAMPOLINE);
                    assert_eq!(result["data"]["files"][0]["url"], SYNTHETIC_FILE_TRAMPOLINE);
                }
            }
            let requests = handle.join().unwrap();
            assert_eq!(
                requests.len(),
                1 + methods.len(),
                "{route}/{reply}: no extra requests"
            );
            assert!(
                requests[0]["target"]
                    .as_str()
                    .unwrap()
                    .starts_with("/open/share/getShareInfoByCodeV2.action?shareCode=AbC123&")
            );
            // Exact endpoint whitelist + canary catches owned dt=3, transfer,
            // listing, and data/Range requests, including a trampoline follow-up.
            assert_public_download_requests(
                &requests[1..],
                "synthetic-file",
                "synthetic-share",
                &methods,
            );
        }
    }
}

#[tokio::test]
async fn synthetic_authoritative_directory_metadata_still_rejects_without_download_callback() {
    for folder_marker in [json!(true), json!(1), json!("1")] {
        let downloads = Cell::new(0);
        let lookups = Cell::new(0);
        let result = service::fast_with_callbacks(
            "https://cloud.189.cn/t/AbC123",
            "",
            "synthetic-root",
            "",
            |_| {
                lookups.set(lookups.get() + 1);
                let info = json!({"fileId":"synthetic-root","fileName":"Directory",
                    "isFolder":folder_marker,"shareId":"synthetic-share"});
                async move { Ok::<_, ()>(Some(info)) }
            },
            |_, _| async {
                panic!("synthetic unprotected share must not check a password");
                #[allow(unreachable_code)]
                Ok::<_, ()>(Value::Null)
            },
            |_, _, _| {
                downloads.set(downloads.get() + 1);
                async { Ok::<_, ()>(Some(SYNTHETIC_FILE_TRAMPOLINE.to_owned())) }
            },
        )
        .await;
        assert_eq!(result.unwrap_err(), public_error(400, "目录无法下载"));
        assert_eq!(lookups.get(), 1);
        assert_eq!(
            downloads.get(),
            0,
            "directory metadata must reject before any download callback"
        );
    }
}

#[test]
fn production_indexed_and_actions_are_send() {
    fn send<T: Send>(_: T) {}
    let client = adapter::Cloud189Client::new(reqwest::Client::new(), json!({}));
    send(service::parse_indexed(
        client.clone(),
        "invalid",
        "",
        &[],
        "",
    ));
    send(service::parse_file(
        client.clone(),
        "s",
        "f",
        "",
        "0",
        "",
        "",
    ));
    send(service::browse_folder_item(
        client, "s", "", "r", "", "1", "",
    ));
}
