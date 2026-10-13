//! Differential HTTP test. Node VM executes the original deployment; the Rust
//! client receives the identical response transcript from a loopback-only stub.
mod support;
pub use jxpan_rust::compat;
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
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(e) => panic!("loopback stub exhausted or accept failed: {e}"),
                }
            };
            requests.push(read_request(&mut stream));
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
async fn deployed_http_contract_differential() {
    let oracle = oracle();
    assert_eq!(
        oracle["source_sha256"],
        "47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118"
    );
    for case in oracle["vectors"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let (base, handle) = stub(case["responses"].as_array().unwrap().clone());
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
            if let Some(expected_error) = case["expected_error"].as_str() {
                let error = result.expect_err("network/parse failures must remain explicit Result");
                assert_eq!(error.kind, expected_error, "{name}");
                outputs.push(Value::Null); // original JS swallows the same failure
            } else {
                outputs.push(result.unwrap_or_else(|e| panic!("{name}: {e}")));
            }
        }
        let requests = handle.join().unwrap();
        assert_eq!(
            json!(requests),
            case["requests"],
            "request transcript: {name}"
        );
        assert_eq!(json!(outputs), case["outputs"], "normalized output: {name}");
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

#[test]
#[should_panic]
fn endpoint_hook_rejects_non_loopback() {
    Cloud189Client::for_loopback_test(http_client(), json!({}), "http://192.0.2.1/");
}
