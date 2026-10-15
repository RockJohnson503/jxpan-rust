//! Offline deployment differential plus independent correctness regressions.
//! The frozen JS corpus is kept intact; known bugs get explicit corrected asserts.
mod support;
pub use jxpan_rust::{compat, settings, tree};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    thread,
    time::{Duration, Instant},
};

// Compile the adapter locally to exercise its private cfg(test) endpoint hook.
// This does not require a production endpoint-injection API or shared-file edits.
mod providers {
    pub use jxpan_rust::providers::cloud189;
}
#[path = "../src/providers/cloud189_client.rs"]
mod adapter;
use adapter::Cloud189Client;

fn oracle() -> Value {
    support::oracle("cloud189-http", include_str!("fixtures/cloud189-http.json"))
}
fn read_request(stream: &mut TcpStream) -> Value {
    // Accepted sockets can inherit O_NONBLOCK on macOS; framing reads are bounded blocking I/O.
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut bytes = Vec::new();
    let end;
    loop {
        let mut buffer = [0; 4096];
        let count = stream.read(&mut buffer).expect("stub request read");
        assert_ne!(count, 0, "unexpected EOF before request headers");
        bytes.extend_from_slice(&buffer[..count]);
        assert!(bytes.len() < 128 * 1024, "stub request limit");
        if let Some(index) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            end = index + 4;
            break;
        }
    }
    let head = std::str::from_utf8(&bytes[..end]).unwrap();
    let mut lines = head.split("\r\n");
    let first: Vec<_> = lines.next().unwrap().split_whitespace().collect();
    let method = first[0].to_owned();
    let target = first[1].to_owned();
    let mut headers = BTreeMap::new();
    for line in lines.filter(|s| !s.is_empty()) {
        let (name, value) = line.split_once(':').unwrap();
        headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
    }
    let length: usize = headers
        .get("content-length")
        .map(|s| s.parse().unwrap())
        .unwrap_or(0);
    assert!(length < 128 * 1024);
    while bytes.len() - end < length {
        let mut buffer = [0; 4096];
        let count = stream.read(&mut buffer).unwrap();
        assert_ne!(count, 0);
        bytes.extend_from_slice(&buffer[..count]);
    }
    // Only HTTP framing/client defaults are excluded. Every deployment header,
    // including credentials, signatures, UA and cookies, is compared exactly.
    for key in ["host", "content-length", "accept-encoding", "connection"] {
        headers.remove(key);
    }
    json!({ "method": method, "target": target, "headers": headers,
        "body": std::str::from_utf8(&bytes[end..end + length]).unwrap() })
}
fn stub(responses: Vec<Value>) -> (String, thread::JoinHandle<Vec<Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut requests = Vec::new();
        for response in responses {
            let canary = response["canary"] == true;
            let wait_deadline = if canary {
                Instant::now() + Duration::from_millis(100)
            } else {
                deadline
            };
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < wait_deadline =>
                    {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && canary => {
                        return requests;
                    }
                    Err(e) => panic!("loopback stub exhausted or accept failed: {e}"),
                }
            };
            requests.push(read_request(&mut stream));
            if response["disconnect"].as_bool() == Some(true) {
                continue; // drop without sending a response
            }
            let body = response["body"].as_str().unwrap();
            let status = response["status"].as_u64().unwrap();
            let mut head = format!(
                "HTTP/1.1 {status} Offline\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
                body.len()
            );
            for cookie in response["cookies"].as_array().unwrap() {
                let value = cookie.as_str().unwrap();
                assert!(!value.contains(['\r', '\n']));
                head.push_str(&format!("Set-Cookie: {value}\r\n"));
            }
            head.push_str("\r\n");
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(body.as_bytes()).unwrap();
        }
        requests
    });
    (url, handle)
}
fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn deployed_http_contract_with_documented_correctness_fixes() {
    let oracle = oracle();
    assert_eq!(
        oracle["source_sha256"],
        "47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118"
    );
    for case in oracle["vectors"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        if name == "v1-empty-stops" {
            // The JS accepts {res_code:0} with no list as empty. That is not a
            // correctness oracle; missing_list_is_an_error covers rejection.
            continue;
        }
        // The frozen helper attempted dt=3 on exhausted shares. Keep its corpus
        // intact, but never script/expect those account-owned requests here.
        let public_indices: Vec<_> = case["requests"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .filter(|(_, request)| params(request).get("dt").is_none_or(|dt| dt != "3"))
            .map(|(index, _)| index)
            .collect();
        let removed_owned = public_indices.len() != case["requests"].as_array().unwrap().len();
        let responses = public_indices
            .iter()
            .map(|index| case["responses"][*index].clone())
            .collect();
        let (base, handle) = stub(responses);
        let mut client =
            Cloud189Client::for_loopback_test(http_client(), case["credentials"].clone(), &base);
        let mut outputs = Vec::new();
        for step in case["actions"].as_array().unwrap() {
            let args: Vec<_> = step["args"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            let result = match step["method"].as_str().unwrap() {
                "get_share_info" => client
                    .get_share_info(args[0])
                    .await
                    .map(|v| v.unwrap_or(Value::Null)),
                "check_access_code" => client.check_access_code(args[0], args[1]).await,
                "list_share_dir" => client
                    .list_share_dir(args[0], args[1], args[2], args[3])
                    .await
                    .map(|v| v.unwrap_or(Value::Null)),
                "download_url" => client
                    .download_url(args[0], args[1])
                    .await
                    .map(|v| v.unwrap_or(Value::Null)),
                _ => panic!("unsupported oracle method"),
            };
            let expected_error = match name {
                "all-fallbacks-null" | "no-signed-credentials" => Some("list rejected"),
                "open-error-app-dt3" => Some("authentication required"),
                _ => case["expected_error"].as_str(),
            };
            if let Some(expected_error) = expected_error {
                let error = result.expect_err("failures must remain explicit Result");
                assert_eq!(error.kind, expected_error, "{name}");
                outputs.push(Value::Null); // original JS swallowed this failure
            } else {
                outputs.push(result.unwrap_or_else(|e| panic!("{name}: {e}")));
            }
        }
        let requests = handle.join().unwrap();
        for (index, request) in requests.iter().enumerate() {
            let original = &case["requests"][public_indices[index]];
            let mut expected_headers = original["headers"].clone();
            let path = request["target"]
                .as_str()
                .unwrap()
                .split('?')
                .next()
                .unwrap();
            if matches!(
                path,
                "/open/file/getFileDownloadUrl.action" | "/getFileDownloadUrl.action"
            ) && let Some(cookie) = case["credentials"]["share_session_cookies"]
                .as_str()
                .filter(|cookie| !cookie.is_empty())
            {
                expected_headers["cookie"] = json!(cookie);
            }
            for field in ["method", "body"] {
                assert_eq!(request[field], original[field], "request {field}: {name}");
            }
            match (name, index) {
                ("share-raw-query-deployment", 0) => assert_eq!(
                    request["target"],
                    "/open/share/getShareInfoByCodeV2.action?shareCode=abc+%2B%26%3D%E4%B8%AD%E6%96%87&noCache=0.25"
                ),
                ("open-raw-query-deployment", 0) => assert_eq!(
                    request["target"],
                    "/open/file/getFileDownloadUrl.action?fileId=file+%2B%26%3D%E4%B8%AD%E6%96%87&dt=1&shareId=share+%2B%26%3D"
                ),
                _ => assert_eq!(
                    request["target"], original["target"],
                    "request target: {name}"
                ),
            }
            if name == "password-cookie-precedence" && (2..=4).contains(&index) {
                assert_eq!(request["headers"]["cookie"], "COOKIE_LOGIN_USER=login==");
                let mut actual_headers = request["headers"].as_object().unwrap().clone();
                let mut original_headers = original["headers"].as_object().unwrap().clone();
                actual_headers.remove("cookie");
                original_headers.remove("cookie");
                assert_eq!(
                    actual_headers, original_headers,
                    "non-cookie headers: {name}"
                );
            } else {
                assert_eq!(
                    request["headers"], expected_headers,
                    "request headers: {name}"
                );
            }
        }
        assert_eq!(
            requests.len(),
            public_indices.len(),
            "request count: {name}"
        );
        if removed_owned {
            assert_eq!(
                outputs,
                vec![Value::Null],
                "owned success must become shared-only failure: {name}"
            );
            continue;
        }
        match name {
            "password-cookie-precedence"
            | "v1-second-mode"
            | "v1-third-mode"
            | "v3-cookie-initialization"
            | "open-fallback"
            | "app-fallback"
            | "json-failure-continues-list-fallback" => {
                // String "1" is a directory, not the JS corpus's false result.
                let corrected = json!([
                    {"fileId":"0", "fileName":"目录", "fileSize":0,
                     "fileType":"", "isFolder":true, "createDate":"yesterday"},
                    {"fileId":"f1", "fileName":"视频.mp4", "fileSize":0,
                     "fileType":"", "isFolder":true, "createDate":""},
                    {"fileId":"f2", "fileName":"子目录", "fileSize":0,
                     "fileType":"", "isFolder":true, "createDate":""}
                ]);
                assert_eq!(
                    outputs.last(),
                    Some(&corrected),
                    "corrected directory flag: {name}"
                );
                if outputs.len() > 1 {
                    let mut earlier =
                        case["outputs"].as_array().unwrap()[..outputs.len() - 1].to_vec();
                    if name == "password-cookie-precedence" {
                        // Absent size is unknown, not the frozen helper's default 0.
                        earlier[0]["fileSize"] = Value::Null;
                    }
                    assert_eq!(
                        &outputs[..outputs.len() - 1],
                        &earlier,
                        "earlier outputs: {name}"
                    );
                }
            }
            "share-nested-non2xx" => assert_eq!(
                outputs,
                vec![json!({
                "fileId":"nested","fileName":"未知文件名","fileSize":0,
                "fileType":"","isFolder":true,"shareId":"sid"})],
                "folder byte-size placeholders normalize to zero; ordinary unknown file sizes stay null"
            ),
            "share-raw-query-deployment" | "share-string-code-rejected" => assert_eq!(
                outputs,
                vec![json!({
                "fileId":"f","fileName":"未知文件名","fileSize":null,
                "fileType":"","isFolder":false,"shareId":""})],
                "unknown file size is not zero"
            ),
            "share-top-bigint" => assert_eq!(
                outputs,
                vec![json!({
                "fileId":"123456789012345678","fileName":"测试","fileSize":0,
                "fileType":"","isFolder":true,"shareId":"987654321098765432"})],
                "an explicit zero is not replaced by a truthy alias"
            ),
            "top-level-fallback-size-nullish" => assert_eq!(
                outputs,
                vec![json!([])],
                "explicit nested empty arrays must not be replaced by top-level data"
            ),
            "open-download" => assert_eq!(
                outputs,
                vec![json!("https://offline.invalid/file?a=1&b=2&c=3")],
                "decode all amp entities, not only the first"
            ),
            "password-string-code-rejected" => assert_eq!(
                json!(outputs),
                json!([{"success":true,"shareId":"s"}]),
                "numeric and string zero both indicate successful password verification"
            ),
            _ => assert_eq!(json!(outputs), case["outputs"], "normalized output: {name}"),
        }
    }
    for case in oracle["merges"].as_array().unwrap() {
        let incoming: Vec<_> = case["incoming"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            adapter::merge_cookies(case["existing"].as_str().unwrap(), &incoming),
            case["output"].as_str().unwrap()
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn open_signatures_keep_leading_zeroes_for_download_and_list() {
    let timestamp = "1700000000123";
    for listing in [false, true] {
        let (token, expected) = (0..1024)
            .find_map(|n| {
                let token = format!("offline-leading-zero-{n}");
                let text = if listing {
                    providers::cloud189::open_list_signing_text(&token, timestamp, "root", "share")
                } else {
                    providers::cloud189::open_download_signing_text(
                        &token, timestamp, "file", "share",
                    )
                };
                let digest = providers::cloud189::md5(&text);
                digest.starts_with('0').then_some((token, digest))
            })
            .expect("bounded synthetic leading-zero signature");
        let replies = if listing {
            let mut replies = vec![response(json!({"res_code":403})); 4];
            replies.push(response(json!({"res_code":0,"fileList":[]})));
            replies
        } else {
            vec![response(
                json!({"fileDownloadUrl":"https://offline.invalid/shared"}),
            )]
        };
        let (base, handle) = stub(replies);
        let mut client = Cloud189Client::for_loopback_test(
            http_client(),
            json!({"open_access_token":token}),
            &base,
        );
        if listing {
            client
                .list_share_dir("share", "root", "", "1")
                .await
                .unwrap();
        } else {
            assert!(
                client
                    .download_open("file", "share")
                    .await
                    .unwrap()
                    .is_some()
            );
        }
        let requests = handle.join().unwrap();
        let request = requests.last().unwrap();
        let actual = if listing {
            params(request)["signature"].clone()
        } else {
            request["headers"]["signature"].as_str().unwrap().to_owned()
        };
        assert_eq!(actual, expected);
        assert_eq!(actual.len(), 32);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn classified_download_errors_are_not_empty_urls_or_reflected_supplier_text() {
    for (code, kind, status) in [
        ("InvalidSessionKey", "authentication required", 401),
        ("InvalidAccessToken", "authentication required", 401),
        ("ShareNotFound", "share unavailable", 404),
        ("ShareAccessCodeNeeded", "access code required", 403),
        ("ShareAccessCodeError", "access code rejected", 403),
    ] {
        let replies = vec![
            response(
                json!({"errorCode":code,"message":"private-secret","fileDownloadUrl":"https://offline.invalid/decoy"})
            );
            2
        ];
        let (base, handle) = stub(replies);
        let client = Cloud189Client::for_loopback_test(http_client(), full_credentials(), &base);
        for result in [
            client.download_open("file", "share").await,
            client.download_app("file", "share", true).await,
        ] {
            let error = result.unwrap_err();
            assert_eq!(error.kind, kind);
            assert_eq!(error.status_code(), status);
            assert!(!format!("{error:?}").contains("private-secret"));
        }
        assert_eq!(handle.join().unwrap().len(), 2);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn folder_size_placeholders_do_not_reject_valid_directories_and_share_mode_is_preserved() {
    for placeholder in [json!(""), json!(-1), Value::Null, json!("not-a-byte-size")] {
        let (base, handle) = stub(vec![
            response(
                json!({"res_code":"0","fileId":"root","shareId":"share","shareMode":3,"isFolder":true,"fileSize":placeholder}),
            ),
            response(
                json!({"res_code":0,"fileListAO":{"count":2,"folderList":[{"id":"directory-1","size":placeholder}],"fileList":[{"id":"directory-2","isFolder":"1","size":placeholder}]}}),
            ),
        ]);
        let mut client = Cloud189Client::for_loopback_test(http_client(), json!({}), &base);
        let info = client.get_share_info("code").await.unwrap().unwrap();
        assert_eq!(info["fileSize"], 0);
        assert_eq!(info["shareMode"], "3");
        let list = client
            .list_share_dir("share", "root", "", info["shareMode"].as_str().unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(list.as_array().unwrap().len(), 2);
        assert!(
            list.as_array()
                .unwrap()
                .iter()
                .all(|item| item["isFolder"] == true && item["fileSize"] == 0)
        );
        let requests = handle.join().unwrap();
        assert_eq!(params(&requests[1])["shareMode"], "3");
    }
    let (base, handle) = stub(vec![response(
        json!({"res_code":0,"fileId":"file","isFolder":false,"fileSize":""}),
    )]);
    let mut client = Cloud189Client::for_loopback_test(http_client(), json!({}), &base);
    assert_eq!(
        client.get_share_info("code").await.unwrap_err().kind,
        "invalid file size"
    );
    assert_eq!(handle.join().unwrap().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn rejected_share_metadata_is_not_misreported_as_a_transport_error() {
    for code in ["ShareNotFound", "FileNotFound"] {
        let (base, handle) = stub(vec![response(
            json!({"res_code":code,"message":"private-url-and-id"}),
        )]);
        let mut client = Cloud189Client::for_loopback_test(http_client(), json!({}), &base);
        let error = client.get_share_info("code").await.unwrap_err();
        assert_eq!(error.status_code(), 404);
        assert!(!format!("{error:?}").contains("private-url"));
        assert_eq!(handle.join().unwrap().len(), 1);
    }
}

fn response(body: Value) -> Value {
    json!({"status":200, "body":body.to_string(), "cookies":[]})
}
fn page(start: usize, length: usize, total: Option<Value>) -> Value {
    let files: Vec<_> = (start..start + length)
        .map(|n| json!({"id":format!("file-{n}"), "name":format!("{n}.mp4")}))
        .collect();
    let mut data = json!({"res_code":0, "fileListAO":{"folderList":[], "fileList":files}});
    if let Some(total) = total {
        data["fileListAO"]["count"] = total;
    }
    response(data)
}
fn params(request: &Value) -> BTreeMap<String, String> {
    url::Url::parse(&format!(
        "http://offline.invalid{}",
        request["target"].as_str().unwrap()
    ))
    .unwrap()
    .query_pairs()
    .into_owned()
    .collect()
}
fn full_credentials() -> Value {
    json!({"session_key":"offline-session", "session_secret":"offline-signing",
           "open_access_token":"offline-open"})
}

#[tokio::test(flavor = "current_thread")]
async fn all_list_routes_paginate_without_changing_route_or_mode() {
    // v1 initial/alternate/third shareMode, v3, Open, App.
    for rejected in 0..6 {
        let mut replies = vec![response(json!({"res_code":403})); rejected];
        replies.extend([
            page(0, 200, Some(json!(201))),
            page(200, 1, Some(json!("201"))),
        ]);
        let (base, handle) = stub(replies);
        let mut client =
            Cloud189Client::for_loopback_test(http_client(), full_credentials(), &base);
        let files = client
            .list_share_dir("share", "root", "password", "1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(files.as_array().unwrap().len(), 201, "route {rejected}");
        assert_eq!(files[200]["fileId"], "file-200");
        let requests = handle.join().unwrap();
        assert_eq!(requests.len(), rejected + 2);
        let first = &requests[rejected];
        let second = &requests[rejected + 1];
        let first_params = params(first);
        let second_params = params(second);
        assert_eq!(first_params["pageNum"], "1");
        assert_eq!(second_params["pageNum"], "2");
        assert_eq!(first_params["pageSize"], "200");
        assert_eq!(second_params["pageSize"], "200");
        let mode = match rejected {
            1 => "2",
            2 => "3",
            _ => "1",
        };
        assert_eq!(first_params["shareMode"], mode);
        assert_eq!(second_params["shareMode"], mode);
        for key in ["fileId", "shareId", "accessCode", "shareMode"] {
            assert_eq!(first_params[key], second_params[key]);
        }
        let first_url = url::Url::parse(&format!(
            "http://offline.invalid{}",
            first["target"].as_str().unwrap()
        ))
        .unwrap();
        let second_url = url::Url::parse(&format!(
            "http://offline.invalid{}",
            second["target"].as_str().unwrap()
        ))
        .unwrap();
        let expected_path = if rejected < 3 {
            "/api/open/share/listShareDir.action"
        } else {
            "/open/share/listShareDir.action"
        };
        assert_eq!(first_url.path(), expected_path);
        assert_eq!(second_url.path(), expected_path);
        for header in ["sessionkey", "signature", "user-agent", "cookie"] {
            assert_eq!(first["headers"][header], second["headers"][header]);
        }
        for key in ["accesstoken", "signature", "timestamp", "clientType"] {
            assert_eq!(first_params.get(key), second_params.get(key));
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn totals_and_full_pages_both_drive_continuation() {
    let transcripts = [
        vec![page(0, 200, None), page(200, 1, None)],
        vec![page(0, 200, None), page(200, 0, None)],
        // A declared total outranks the short-page heuristic.
        vec![page(0, 1, Some(json!(2))), page(1, 1, Some(json!(2)))],
        vec![page(0, 200, Some(json!(200)))],
    ];
    for (replies, expected) in transcripts.into_iter().zip([201, 200, 2, 200]) {
        let requests_expected = replies.len();
        let (base, handle) = stub(replies);
        let mut client = Cloud189Client::for_loopback_test(http_client(), json!({}), &base);
        let files = client
            .list_share_dir("share", "root", "", "1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(files.as_array().unwrap().len(), expected);
        assert_eq!(handle.join().unwrap().len(), requests_expected);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn later_page_failure_never_returns_partial_success_or_switches_route() {
    for (second, expected) in [
        (response(json!({"res_code":403})), "list rejected"),
        (response(json!({"res_code":0})), "missing file list"),
        (
            response(json!({"res_code":0,"fileListAO":{"fileList":{}}})),
            "invalid file list",
        ),
        (
            json!({"status":200,"body":"<html>bad</html>","cookies":[]}),
            "invalid JSON",
        ),
        (json!({"disconnect":true}), "transport failure"),
        (
            json!({"status":500,"body":"{\"res_code\":0,\"fileList\":[]}","cookies":[]}),
            "HTTP failure",
        ),
        (page(200, 0, Some(json!(201))), "incomplete file list"),
        (page(200, 1, Some(json!(202))), "inconsistent list total"),
        (page(0, 200, Some(json!(201))), "repeated list entry/page"),
    ] {
        let (base, handle) = stub(vec![page(0, 200, Some(json!(201))), second]);
        let mut client =
            Cloud189Client::for_loopback_test(http_client(), full_credentials(), &base);
        let error = client
            .list_share_dir("share", "root", "", "1")
            .await
            .unwrap_err();
        assert_eq!(error.operation, "list_v1");
        assert_eq!(error.kind, expected);
        let requests = handle.join().unwrap();
        assert_eq!(
            requests.len(),
            2,
            "later failure must not restart fallbacks"
        );
        assert_eq!(params(&requests[1])["pageNum"], "2");
        assert_eq!(params(&requests[1])["shareMode"], "1");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn repeated_or_overlapping_pages_fail_explicitly() {
    for second in [page(0, 200, None), page(199, 2, None)] {
        let (base, handle) = stub(vec![page(0, 200, None), second]);
        let mut client = Cloud189Client::for_loopback_test(http_client(), json!({}), &base);
        let error = client
            .list_share_dir("share", "root", "", "1")
            .await
            .unwrap_err();
        assert_eq!(error.kind, "repeated list entry/page");
        assert_eq!(handle.join().unwrap().len(), 2);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn list_resource_limits_are_errors_not_truncation() {
    for replies in [
        vec![page(0, 1, Some(json!(20_001)))],
        (0..100).map(|n| page(n, 1, Some(json!(101)))).collect(),
    ] {
        let requests_expected = replies.len();
        // A sparse reported total exercises the page cap without first hitting
        // the independent accumulated-memory bound.
        let (base, handle) = stub(replies);
        let mut client = Cloud189Client::for_loopback_test(http_client(), json!({}), &base);
        let error = client
            .list_share_dir("share", "root", "", "1")
            .await
            .unwrap_err();
        assert_eq!(
            error.kind,
            if requests_expected == 1 {
                "list item limit exceeded"
            } else {
                "list page limit exceeded"
            }
        );
        assert_eq!(handle.join().unwrap().len(), requests_expected);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn missing_list_is_an_error_and_explicit_empty_arrays_are_valid() {
    for data in [json!({"res_code":0}), json!({"res_code":0,"fileListAO":{}})] {
        let (base, handle) = stub(vec![response(data); 4]);
        let mut client = Cloud189Client::for_loopback_test(http_client(), json!({}), &base);
        assert_eq!(
            client
                .list_share_dir("share", "root", "", "1")
                .await
                .unwrap_err()
                .kind,
            "missing file list"
        );
        assert_eq!(handle.join().unwrap().len(), 4);
    }
    for data in [
        json!({"res_code":0,"fileList":[]}),
        json!({"res_code":"0","fileListAO":{"folderList":[],"fileList":[]},"fileList":[{"id":"ignored"}]}),
    ] {
        let (base, handle) = stub(vec![response(data)]);
        let mut client = Cloud189Client::for_loopback_test(http_client(), json!({}), &base);
        assert_eq!(
            client
                .list_share_dir("share", "root", "", "1")
                .await
                .unwrap(),
            Some(json!([]))
        );
        assert_eq!(handle.join().unwrap().len(), 1);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn explicit_zero_total_allows_omitted_empty_arrays_but_never_missing_or_conflicting_lists() {
    // Independent, redacted shape of a real v3 supplier empty-directory response.
    // A rejected authenticated web route must not prevent this read-only fallback.
    let (base, handle) = stub(vec![
        response(json!({"res_code":"InvalidSessionKey"})),
        response(json!({"res_code":"InvalidSessionKey"})),
        response(json!({"res_code":"InvalidSessionKey"})),
        response(json!({"res_code":0,"fileListAO":{"count":0,"fileListSize":0}})),
    ]);
    let mut client = Cloud189Client::for_loopback_test(http_client(), json!({}), &base);
    assert_eq!(
        client
            .list_share_dir("share", "root", "synthetic-password", "1")
            .await
            .unwrap(),
        Some(json!([]))
    );
    assert_eq!(handle.join().unwrap().len(), 4);

    for data in [
        json!({"res_code":0,"fileListAO":{"count":"0","fileListSize":"0"}}),
        json!({"res_code":0,"totalCount":0}),
        json!({"res_code":0,"fileListAO":{"count":0},"fileList":[{"id":"unrelated-top-level"}]}),
    ] {
        let (base, handle) = stub(vec![response(data)]);
        let mut client = Cloud189Client::for_loopback_test(http_client(), json!({}), &base);
        assert_eq!(
            client
                .list_share_dir("share", "root", "", "1")
                .await
                .unwrap(),
            Some(json!([]))
        );
        assert_eq!(handle.join().unwrap().len(), 1);
    }
    for data in [
        json!({"res_code":0,"fileListAO":{"count":1}}),
        json!({"res_code":0,"fileListAO":{"count":0,"fileListSize":1}}),
        json!({"res_code":0,"fileListAO":{"count":0,"fileListSize":-1}}),
        json!({"res_code":0,"fileListAO":{"count":0,"fileListSize":"invalid"}}),
        json!({"res_code":0,"fileListAO":{"count":0,"fileList":null}}),
        json!({"res_code":0,"fileListAO":{"count":0},"total":1}),
        json!({"res_code":0,"fileListAO":{"count":false}}),
        json!({"res_code":0,"fileListAO":{"count":0.5}}),
        json!({"res_code":403,"fileListAO":{"count":0}}),
    ] {
        let (base, handle) = stub(vec![response(data); 4]);
        let mut client = Cloud189Client::for_loopback_test(http_client(), json!({}), &base);
        assert!(
            client
                .list_share_dir("share", "root", "", "1")
                .await
                .is_err()
        );
        assert_eq!(handle.join().unwrap().len(), 4);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn dynamic_queries_encode_reserved_characters_without_injection() {
    let input = "x +&=中文#fragment?slash/";
    let (base, handle) = stub(vec![
        response(json!({"res_code":0,"fileId":"root"})),
        response(json!({"fileDownloadUrl":"//offline.invalid/signed?x=1&amp;y=2&amp;z=3"})),
        page(0, 0, None),
    ]);
    let mut client = Cloud189Client::for_loopback_test(http_client(), full_credentials(), &base);
    client.get_share_info(input).await.unwrap();
    assert_eq!(
        client.download_open(input, input).await.unwrap().as_deref(),
        Some("https://offline.invalid/signed?x=1&y=2&z=3")
    );
    client
        .list_share_dir(input, input, input, input)
        .await
        .unwrap();
    let requests = handle.join().unwrap();
    let info = params(&requests[0]);
    assert_eq!(info.len(), 2);
    assert_eq!(info["shareCode"], input);
    let download = params(&requests[1]);
    assert_eq!(download.len(), 3);
    assert_eq!(download["fileId"], input);
    assert_eq!(download["shareId"], input);
    assert_eq!(download["dt"], "1");
    let list = params(&requests[2]);
    for key in [
        "shareId",
        "fileId",
        "shareDirFileId",
        "accessCode",
        "shareMode",
    ] {
        assert_eq!(list[key], input);
    }
    for request in requests {
        assert!(!request["target"].as_str().unwrap().contains('#'));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn captured_cookie_values_keep_equals_and_attribute_boundaries() {
    let mut info = response(json!({"res_code":0,"fileId":"root"}));
    info["cookies"] = json!([
        "COOKIE_LOGIN_USER===; Max-Age=10; Path=/",
        "DEAD=x; Max-Age=-2; Path=/",
        "ZERO=x; Max-Age=0; Path=/",
        "KEEP=xmax-age=0==; Max-Age=10; Path=/",
        "OLD=x; Expires=Thu, 01 Jan 1970 00:00:00 GMT, SHARE=a=b==; Expires=Tue, 01 Jan 2030 00:00:00 GMT"
    ]);
    let mut checked = response(json!({"res_code":0,"shareId":"share"}));
    checked["cookies"] = info["cookies"].clone();
    let (base, handle) = stub(vec![
        info,
        checked,
        response(json!({"res_code":403})),
        response(json!({"res_code":403})),
        response(json!({"res_code":403})),
        page(0, 0, None),
    ]);
    let mut client = Cloud189Client::for_loopback_test(http_client(), json!({}), &base);
    client.get_share_info("share").await.unwrap();
    client.check_access_code("share", "password").await.unwrap();
    client
        .list_share_dir("share", "root", "password", "1")
        .await
        .unwrap();
    let requests = handle.join().unwrap();
    for request in &requests[2..5] {
        assert_eq!(request["headers"]["cookie"], "COOKIE_LOGIN_USER===");
    }
    assert_eq!(
        requests[5]["headers"]["cookie"],
        "COOKIE_LOGIN_USER===; KEEP=xmax-age=0==; SHARE=a=b=="
    );
}

#[tokio::test(flavor = "current_thread")]
async fn transport_failure_is_explicit_and_redacted() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read_request(&mut stream); // drop without a response
    });
    let mut client = Cloud189Client::for_loopback_test(
        http_client(),
        json!({"session_key":"offline-secret-do-not-log"}),
        &base,
    );
    let error = client.get_share_info("AbC123").await.unwrap_err();
    assert_eq!(error.operation, "get_share_info");
    assert_eq!(error.kind, "transport failure");
    assert!(!format!("{error:?} {error}").contains("offline-secret"));
    server.join().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn share_and_list_ids_preserve_zero_and_reject_invalid_values() {
    for nested in [false, true] {
        for id in [
            json!(0),
            json!(-1),
            json!("123456789012345678"),
            json!("file"),
        ] {
            let info = json!({"fileId":id,"shareId":0,"fileSize":0,"size":42});
            let body = if nested {
                json!({"fileInfoAO":info})
            } else {
                let mut body = info;
                body["res_code"] = json!(0);
                body
            };
            let (base, handle) = stub(vec![response(body)]);
            let mut client = Cloud189Client::for_loopback_test(http_client(), json!({}), &base)
                .with_limit(settings::UpstreamLimit::new(2));
            assert_eq!(client.concurrency(), 2);
            let result = client.get_share_info("share").await.unwrap().unwrap();
            assert_eq!(
                result["fileId"],
                id.as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| id.to_string())
            );
            assert_eq!(result["shareId"], "0");
            assert_eq!(result["fileSize"], 0);
            assert_eq!(handle.join().unwrap().len(), 1);
        }
    }
    for id in [
        json!(""),
        json!("  "),
        json!(false),
        json!([]),
        json!({}),
        json!(1.5),
    ] {
        let (base, handle) = stub(vec![response(json!({"res_code":0,"fileId":id}))]);
        let mut client = Cloud189Client::for_loopback_test(http_client(), json!({}), &base);
        assert_eq!(
            client.get_share_info("share").await.unwrap_err().kind,
            "invalid file ID"
        );
        assert_eq!(handle.join().unwrap().len(), 1);
        let (base, handle) = stub(vec![
            response(json!({"res_code":0,"fileList":[{"id":id}]}));
            4
        ]);
        let mut client = Cloud189Client::for_loopback_test(http_client(), json!({}), &base);
        assert_eq!(
            client
                .list_share_dir("s", "r", "", "1")
                .await
                .unwrap_err()
                .kind,
            "invalid list entry"
        );
        assert_eq!(handle.join().unwrap().len(), 4);
    }
    let (base, handle) = stub(vec![
        response(json!({"res_code":0,"fileList":[
            {"id":0,"fileId":"ignored","size":0,"fileSize":42},
            {"fileId":-1}, {"id":"null-size","size":null,"fileSize":"9007199254740993"}
        ]})),
        response(json!({"res_code":0,"shareId":0})),
    ]);
    let mut client = Cloud189Client::for_loopback_test(http_client(), json!({}), &base);
    let list = client
        .list_share_dir("s", "r", "", "1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(list[0]["fileId"], "0");
    assert_eq!(list[0]["fileSize"], 0);
    assert_eq!(list[1]["fileId"], "-1");
    assert!(list[1]["fileSize"].is_null());
    assert_eq!(list[2]["fileSize"], "9007199254740993");
    assert_eq!(
        client.check_access_code("share", "pwd").await.unwrap(),
        json!({"success":true,"shareId":"0"})
    );
    assert_eq!(handle.join().unwrap().len(), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn share_sizes_preserve_unknown_and_reject_invalid_counts() {
    for (fields, expected) in [
        (json!({}), Value::Null),
        (json!({"fileSize":null,"size":null}), Value::Null),
        (json!({"fileSize":0,"size":9}), json!(0)),
        (
            json!({"fileSize":null,"size":"9007199254740993"}),
            json!("9007199254740993"),
        ),
    ] {
        let mut body = fields;
        body["res_code"] = json!(0);
        body["fileId"] = json!("f");
        let (base, handle) = stub(vec![response(body)]);
        let mut client = Cloud189Client::for_loopback_test(http_client(), json!({}), &base);
        assert_eq!(
            client.get_share_info("share").await.unwrap().unwrap()["fileSize"],
            expected
        );
        assert_eq!(handle.join().unwrap().len(), 1);
    }
    for size in [json!(-1), json!(1.5), json!("NaN"), json!(false)] {
        let (base, handle) = stub(vec![response(
            json!({"res_code":0,"fileId":"f","fileSize":size}),
        )]);
        let mut client = Cloud189Client::for_loopback_test(http_client(), json!({}), &base);
        assert_eq!(
            client.get_share_info("share").await.unwrap_err().kind,
            "invalid file size"
        );
        assert_eq!(handle.join().unwrap().len(), 1);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn exhausted_shared_downloads_and_owned_selector_never_send_dt3() {
    let (base, handle) = stub(vec![
        response(json!({"res_code":403})),
        response(json!({"res_code":403})),
        // A live canary catches attempted dt=3 instead of hiding it behind a
        // swallowed connection-refused error after the two shared responses.
        json!({"canary":true,"status":200,"cookies":[],"body":"{\"fileDownloadUrl\":\"https://offline.invalid/forbidden-owned\"}"}),
    ]);
    let mut client = Cloud189Client::for_loopback_test(http_client(), full_credentials(), &base);
    assert_eq!(client.download_app("f", "s", false).await.unwrap(), None);
    assert_eq!(client.download_url("f", "s").await.unwrap(), None);
    let requests = handle.join().unwrap();
    assert_eq!(requests.len(), 2);
    for request in requests {
        let query = params(&request);
        assert_eq!(query["dt"], "1");
        assert_eq!(query["shareId"], "s");
        assert!(!query.contains_key("flag"));
        assert!(request["headers"]["range"].is_null());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn shared_download_body_holds_shared_limit_until_complete_or_cancelled() {
    for cancel in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}/", listener.local_addr().unwrap());
        let (headers_tx, headers_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let server = thread::spawn(move || {
            let accept = || {
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    match listener.accept() {
                        Ok((stream, _)) => return stream,
                        Err(error)
                            if error.kind() == std::io::ErrorKind::WouldBlock
                                && Instant::now() < deadline =>
                        {
                            thread::sleep(Duration::from_millis(1))
                        }
                        Err(error) => panic!("loopback accept: {error}"),
                    }
                }
            };
            let mut first = accept();
            let first_request = read_request(&mut first);
            let body = r#"{"fileDownloadUrl":"https://offline.invalid/first"}"#;
            first.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{{", body.len()).as_bytes()).unwrap();
            headers_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            let written = first.write_all(&body.as_bytes()[1..]);
            if !cancel {
                written.unwrap();
            }
            drop(first);
            let mut second = accept();
            let second_request = read_request(&mut second);
            let body = r#"{"fileDownloadUrl":"https://offline.invalid/second"}"#;
            second.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).unwrap();
            vec![first_request, second_request]
        });
        let limiter = settings::UpstreamLimit::new(1);
        let first = Cloud189Client::for_loopback_test(http_client(), full_credentials(), &base)
            .with_limit(limiter.clone());
        let second = Cloud189Client::for_loopback_test(http_client(), full_credentials(), &base)
            .with_limit(limiter.clone());
        let task = tokio::spawn(async move { first.download_app("f", "s", true).await });
        tokio::time::timeout(Duration::from_secs(5), headers_rx)
            .await
            .unwrap()
            .unwrap();
        // Inspect the actual budget, not merely a second socket waiting for a
        // response: the stub deliberately has not sent the complete JSON body.
        assert!(
            tokio::time::timeout(Duration::from_millis(20), limiter.acquire())
                .await
                .is_err()
        );
        let mut waiting = Box::pin(second.download_open("f", "s"));
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut waiting)
                .await
                .is_err(),
            "headers alone must not release the shared I/O budget"
        );
        if cancel {
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
            release_tx.send(()).unwrap();
        } else {
            release_tx.send(()).unwrap();
            assert_eq!(
                task.await.unwrap().unwrap().as_deref(),
                Some("https://offline.invalid/first")
            );
        }
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), waiting)
                .await
                .unwrap()
                .unwrap()
                .as_deref(),
            Some("https://offline.invalid/second")
        );
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(
            requests[0]["target"]
                .as_str()
                .unwrap()
                .starts_with("/getFileDownloadUrl.action")
        );
        assert!(
            requests[1]["target"]
                .as_str()
                .unwrap()
                .starts_with("/open/file/getFileDownloadUrl.action")
        );
        for request in requests {
            assert_eq!(params(&request)["dt"], "1");
        }
    }
}

#[test]
#[should_panic]
fn endpoint_hook_rejects_non_loopback() {
    Cloud189Client::for_loopback_test(http_client(), json!({}), "http://192.0.2.1/");
}
