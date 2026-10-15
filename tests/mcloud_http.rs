//! Offline differential tests: original JS uses strict-memory fetch; Rust HTTP uses
//! numeric loopback only. No real account or production-network request is made.
mod support;
use jxpan_rust::providers::mcloud;
pub use jxpan_rust::{compat, settings, tree};
// Include directly so this delegated file can be tested before shared mod.rs wiring.
#[path = "../src/providers/mcloud_client.rs"]
mod mcloud_client;
use mcloud_client::*;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

fn oracle() -> &'static Value {
    static ORACLE: OnceLock<Value> = OnceLock::new();
    ORACLE.get_or_init(|| {
        let value = support::oracle("mcloud-http", include_str!("fixtures/mcloud-http.json"));
        assert_eq!(value["deploymentSha256"], mcloud::DEPLOYMENT_SHA256);
        value
    })
}

#[test]
fn original_webcrypto_double_padding_fixed_iv_vectors() {
    let vectors = oracle()["aes"].as_array().unwrap();
    assert_eq!(vectors.len(), 87);
    for (index, vector) in vectors.iter().enumerate() {
        let iv: [u8; 16] = std::array::from_fn(|i| vector["iv"][i].as_u64().unwrap() as u8);
        let encrypted = encrypt139_with_iv(vector["text"].as_str().unwrap(), iv);
        assert_eq!(
            encrypted, vector["encrypted"],
            "AES encryption vector {index}"
        );
        assert_eq!(
            decrypt139(&encrypted).unwrap(),
            vector["decrypted"],
            "AES decryption vector {index}"
        );
    }
    // atob compatibility accepts whitespace, absent padding and nonzero trailing bits.
    let encrypted = vectors[1]["encrypted"].as_str().unwrap();
    assert_eq!(
        decrypt139(&format!(" \n{}\t", encrypted.trim_end_matches('='))).unwrap(),
        "x"
    );
    for invalid in ["", "not base64", "YQ==", "AAAAAAAAAAAAAAAAAAAAAA=="] {
        assert_eq!(decrypt139(invalid), Err(MobileCloudError::Crypto));
    }
}

fn original_headers(vector: &Value) -> BTreeMap<String, String> {
    vector["captured"]["headers"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(key, value)| (key.to_ascii_lowercase(), value.as_str().unwrap().to_owned()))
        .collect()
}

#[test]
fn literal_templates_all_headers_and_empty_field_semantics() {
    let mut compared = 0;
    for (index, vector) in oracle()["requests"].as_array().unwrap().iter().enumerate() {
        if vector["captured"].is_null() {
            continue;
        }
        let auth = vector["authorization"].as_str().unwrap();
        let account = mcloud::extract_account_from_auth(&json!(auth)).unwrap();
        let args = vector["args"].as_array().unwrap();
        let body = match vector["method"].as_str().unwrap() {
            "getShareFiles" => list_request_body(
                &account,
                args[0].as_str().unwrap(),
                args[1].as_str().unwrap(),
                args[2].as_str().unwrap(),
            ),
            "getDownloadUrl" => download_request_body(
                &account,
                args[0].as_str().unwrap(),
                args[1].as_str().unwrap(),
            ),
            _ => unreachable!(),
        };
        assert_eq!(
            body, vector["captured"]["body"],
            "literal request vector {index}"
        );
        let headers = build_headers(auth).unwrap();
        let actual: BTreeMap<_, _> = headers
            .iter()
            .map(|(key, value)| (key.as_str().to_owned(), value.to_str().unwrap().to_owned()))
            .collect();
        assert_eq!(
            actual,
            original_headers(vector),
            "complete header vector {index}"
        );
        if let Some(auth) = headers.get("authorization") {
            assert!(auth.is_sensitive());
        }
        compared += 1;
    }
    assert!(compared >= 45);
}

#[test]
fn response_normalization_against_original_including_directory_and_download_rescue() {
    let mut rescue = 0;
    for (index, vector) in oracle()["requests"].as_array().unwrap().iter().enumerate() {
        if vector["captured"].is_null() || vector["status"] != 200 || vector["raw"] == "" {
            continue;
        }
        let text = vector["raw"].as_str().unwrap();
        match vector["method"].as_str().unwrap() {
            "getShareFiles" => {
                let actual =
                    normalize_list_response(text).expect("directory response including rescue");
                assert_eq!(
                    json!(actual),
                    vector["expected"],
                    "list conversion {index} ({})",
                    vector["directoryRescueProbe"]
                );
                if !vector["directoryRescueProbe"].is_null() {
                    rescue += 1;
                }
            }
            "getDownloadUrl" => {
                assert_eq!(
                    json!(normalize_download_response(text).unwrap_or(None)),
                    vector["expected"],
                    "download conversion {index}"
                );
            }
            _ => unreachable!(),
        }
    }
    assert_eq!(
        rescue, 37,
        "all named directory catch and negative probes compared"
    );
}

#[test]
fn oversized_directory_rescue_is_not_scanned() {
    // Safety boundary, not an original-class parity claim for oversized bodies.
    let mut body = "!".repeat(8 * 1024 * 1024);
    body.push_str(r#""coLst":[{"coID":"should-not-rescue"}]"#);
    assert_eq!(normalize_list_response(&body).unwrap(), Vec::<Value>::new());
}

#[derive(Debug)]
struct Captured {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: String,
}
fn capture(stream: &mut TcpStream) -> Captured {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut bytes = Vec::new();
    let split = loop {
        let mut buffer = [0; 4096];
        let count = stream.read(&mut buffer).unwrap();
        assert_ne!(count, 0, "request ended before headers");
        bytes.extend_from_slice(&buffer[..count]);
        assert!(bytes.len() < 1024 * 1024, "synthetic request cap");
        if let Some(index) = bytes.windows(4).position(|x| x == b"\r\n\r\n") {
            break index;
        }
    };
    let head = String::from_utf8(bytes[..split].to_vec()).unwrap();
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next().unwrap().split_whitespace();
    let method = request_line.next().unwrap().to_owned();
    let path = request_line.next().unwrap().to_owned();
    let headers: BTreeMap<String, String> = lines
        .map(|line| {
            let (key, value) = line.split_once(':').unwrap();
            (key.to_ascii_lowercase(), value.trim().to_owned())
        })
        .collect();
    let length: usize = headers["content-length"].parse().unwrap();
    assert!(length < 1024 * 1024);
    let body_start = split + 4;
    while bytes.len() < body_start + length {
        let mut buffer = [0; 4096];
        let count = stream.read(&mut buffer).unwrap();
        assert_ne!(count, 0, "request ended before body");
        bytes.extend_from_slice(&buffer[..count]);
    }
    Captured {
        method,
        path,
        headers,
        body: String::from_utf8(bytes[body_start..body_start + length].to_vec()).unwrap(),
    }
}
// Every accept is bounded, even when a regression short-circuits a request. A
// dropped test future can also stop a scripted server without a background daemon.
fn accept_before(
    listener: &TcpListener,
    stop: Option<&AtomicBool>,
) -> Option<(TcpStream, SocketAddr)> {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if stop.is_some_and(|stop| stop.load(Ordering::SeqCst)) || Instant::now() >= deadline {
            return None;
        }
        match listener.accept() {
            Ok((stream, peer)) => {
                stream.set_nonblocking(false).unwrap();
                return Some((stream, peer));
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(2))
            }
            Err(error) => panic!("loopback accept failed: {error}"),
        }
    }
}

fn test_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .unwrap()
}

#[tokio::test]
async fn frozen_legacy_encrypted_loopback_http_matches_unchanged_oracle() {
    let vectors = oracle()["requests"].as_array().unwrap();
    let network: Vec<Value> = vectors
        .iter()
        .filter(|v| !v["captured"].is_null())
        .cloned()
        .collect();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server_vectors = network.clone();
    let server = thread::spawn(move || {
        let mut captured = Vec::new();
        for vector in &server_vectors {
            let Some((mut stream, peer)) = accept_before(&listener, None) else {
                break;
            };
            assert!(peer.ip().is_loopback());
            captured.push(capture(&mut stream));
            let wire = vector["wire"].as_str().unwrap();
            write!(
                stream,
                "HTTP/1.1 {} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                vector["status"].as_u64().unwrap(),
                wire.len(),
                wire
            )
            .unwrap();
        }
        captured
    });
    // One caller-supplied reqwest client reused by every MobileCloudClient clone.
    let shared = test_client();
    for (index, vector) in network.iter().enumerate() {
        let client = MobileCloudClient::new_for_loopback(
            shared.clone(),
            vector["authorization"].as_str().unwrap().to_owned(),
            address,
        )
        .unwrap();
        let args = vector["args"].as_array().unwrap();
        let actual = if vector["method"] == "getShareFiles" {
            let result = client
                .list_files_legacy(
                    args[0].as_str().unwrap(),
                    args[1].as_str().unwrap(),
                    args[2].as_str().unwrap(),
                )
                .await;
            if vector["status"] != 200 {
                assert_eq!(
                    result,
                    Err(MobileCloudError::Http(
                        vector["status"].as_u64().unwrap() as u16
                    ))
                );
            }
            json!(result.unwrap_or_default())
        } else {
            let result = client
                .download_url_legacy(args[0].as_str().unwrap(), args[1].as_str().unwrap())
                .await;
            if vector["status"] != 200 {
                assert_eq!(
                    result,
                    Err(MobileCloudError::Http(
                        vector["status"].as_u64().unwrap() as u16
                    ))
                );
            }
            json!(result.unwrap_or(None))
        };
        assert_eq!(actual, vector["expected"], "HTTP response vector {index}");
    }
    let captured = server.join().unwrap();
    assert_eq!(captured.len(), network.len());
    for (index, (actual, vector)) in captured.iter().zip(&network).enumerate() {
        assert_eq!(actual.method, "POST");
        assert_eq!(
            actual.path,
            vector["captured"]["url"]
                .as_str()
                .unwrap()
                .strip_prefix("https://share-kd-njs.yun.139.com")
                .unwrap()
        );
        assert_eq!(
            decrypt139(&actual.body).unwrap(),
            vector["captured"]["body"],
            "HTTP body {index}"
        );
        for (key, value) in original_headers(vector) {
            assert_eq!(
                actual.headers.get(&key),
                Some(&value),
                "HTTP header {key}, vector {index}"
            );
        }
    }
    for vector in vectors.iter().filter(|v| v["captured"].is_null()) {
        // No listener is running: calls must short-circuit before transport.
        let client = MobileCloudClient::new_for_loopback(
            shared.clone(),
            vector["authorization"].as_str().unwrap().to_owned(),
            address,
        )
        .unwrap();
        let args = vector["args"].as_array().unwrap();
        let actual = if vector["method"] == "getShareFiles" {
            json!(
                client
                    .list_files_legacy(
                        args[0].as_str().unwrap(),
                        args[1].as_str().unwrap(),
                        args[2].as_str().unwrap()
                    )
                    .await
                    .unwrap()
            )
        } else {
            json!(
                client
                    .download_url_legacy(args[0].as_str().unwrap(), args[1].as_str().unwrap())
                    .await
                    .unwrap()
            )
        };
        assert_eq!(actual, vector["expected"]);
    }
}

#[tokio::test]
async fn single_padding_provider_json_preserves_bracketed_mp4s_through_full_tree_and_download() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use cbc::cipher::{BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};

    use jxpan_rust::providers::mcloud_service::{ServiceConfig, parse_with_callbacks};

    let root = json!({"resultCode":"0","success":true,"data":{
        "caLst":[{"caID":"show","caName":"Synthetic Show"}],"coLst":[],
        "description":"x".repeat(256)}});
    let show = json!({"resultCode":"0","success":true,"data":{
        "caLst":[{"caID":"season-1","caName":"Season 1"}],"coLst":[],
        "description":"x".repeat(256)}});
    let season = json!({"resultCode":"0","success":true,"data":{
        "caLst":[],"coLst":[
            {"coID":"episode-01","coName":"[Group] Synthetic S01E01 [1080p].mp4","coSize":1234,
             "coType":0,"path":"/synthetic/episode-01","meta":{"labels":["[one]","two"]}},
            {"coID":"episode-02","coName":"[Group] Synthetic S01E02 [1080p].mp4","coSize":5678,
             "coType":0,"path":"/synthetic/episode-02","meta":{"labels":["[one]","two"]}}],
        "description":"padding must not eat this complete JSON suffix"}});
    let download = json!({"success":true,"data":{"redrUrl":"https://download.invalid/synthetic"}});
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = thread::spawn(move || {
        let mut calls = Vec::new();
        for body in [root, show, season, download] {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let (mut stream, peer) = loop {
                match listener.accept() {
                    Ok(connection) => break connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if std::time::Instant::now() >= deadline {
                            return calls;
                        }
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("loopback accept failed: {error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            assert!(peer.ip().is_loopback());
            let request = capture(&mut stream);
            calls.push(serde_json::from_str::<Value>(&decrypt139(&request.body).unwrap()).unwrap());
            // Supplier response: exactly one standard byte-based CBC PKCS#7 layer.
            let iv = [9u8; 16];
            let encrypted =
                cbc::Encryptor::<aes::Aes128>::new(b"PVGDwmcvfs1uV3d1".into(), (&iv).into())
                    .encrypt_padded_vec_mut::<Pkcs7>(body.to_string().as_bytes());
            let wire = STANDARD.encode([iv.as_slice(), encrypted.as_slice()].concat());
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                wire.len(),
                wire
            )
            .unwrap();
        }
        calls
    });
    let client = MobileCloudClient::new_for_loopback(
        test_client(),
        format!("Basic {}", STANDARD.encode("pc:synthetic:token")),
        address,
    )
    .unwrap();
    let list_client = client.clone();
    let download_calls = std::cell::Cell::new(0);
    let value = parse_with_callbacks(
        ServiceConfig {
            enabled: true,
            authorization_configured: true,
        },
        "https://yun.139.com/shareweb/#/w/i/Synthetic123",
        &[],
        "",
        move |id| {
            let client = list_client.clone();
            async move {
                client
                    .list_files("Synthetic123", "", &id)
                    .await
                    .map_err(|_| ())
            }
        },
        |_| {
            download_calls.set(download_calls.get() + 1);
            async { Err::<Option<String>, ()>(()) }
        },
    )
    .await;
    // Run the fourth synthetic call before assertions so the loopback server drains
    // even when the regression is intentionally red.
    let url = client.download_url("Synthetic123", "episode-01").await;
    let calls = server.join().unwrap();
    assert_eq!(value["code"], 200);
    assert_eq!(
        value["data"]["tree"],
        json!({"移动云盘分享":{"Synthetic Show":{"Season 1":{
            "[Group] Synthetic S01E01 [1080p].mp4":{"id":"episode-01","size":1234},
            "[Group] Synthetic S01E02 [1080p].mp4":{"id":"episode-02","size":5678}
        }}}})
    );
    assert_eq!(
        download_calls.get(),
        0,
        "tree listing must not prefetch downloads"
    );
    assert_eq!(
        url.unwrap().as_deref(),
        Some("https://download.invalid/synthetic")
    );
    assert_eq!(
        calls.len(),
        4,
        "three directory requests and one explicit download request"
    );
    let ids: Vec<_> = calls[..3]
        .iter()
        .map(|c| c["getOutLinkInfoReq"]["pCaID"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["root", "show", "season-1"]);
    assert_eq!(
        calls[3]["dlFromOutLinkReqV3"]["coIDLst"]["item"],
        json!(["episode-01"])
    );
}

// Production scenarios are separate from the frozen JS oracle above: corrected
// JSON escaping, strict completeness and URL validation are intentionally not parity.
struct ScriptServer {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<Vec<Captured>>>,
}
impl ScriptServer {
    fn new(responses: Vec<(u16, String)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = thread::spawn(move || {
            let mut calls = Vec::new();
            for (status, wire) in responses {
                let Some((mut stream, peer)) = accept_before(&listener, Some(&stopped)) else {
                    break;
                };
                assert!(peer.ip().is_loopback());
                calls.push(capture(&mut stream));
                write!(stream, "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{wire}", wire.len()).unwrap();
            }
            calls
        });
        Self {
            address,
            stop,
            thread: Some(thread),
        }
    }
    fn client(&self, authorization: &str) -> MobileCloudClient {
        let client =
            MobileCloudClient::new_for_loopback(test_client(), authorization.into(), self.address)
                .unwrap()
                .with_limit(settings::UpstreamLimit::new(2));
        assert_eq!(client.concurrency(), 2);
        client
    }
    fn finish(mut self) -> Vec<Captured> {
        self.stop.store(true, Ordering::SeqCst);
        self.thread.take().unwrap().join().unwrap()
    }
}
impl Drop for ScriptServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
fn ok_page(cas: Value, cos: Value) -> Value {
    json!({"resultCode":"0","data":{"caLst":cas,"coLst":cos}})
}
fn files(start: usize, count: usize) -> Value {
    json!((start..start + count).map(|id| json!({"coID":format!("f{id}"),
        "coName":format!("file-{id}.mp4"),"coType":"0","isDir":"false","coSize":"9007199254740993"})).collect::<Vec<_>>())
}
fn catalogs(start: usize, count: usize) -> Value {
    json!(
        (start..start + count)
            .map(|id| json!({"caID":format!("d{id}"),
        "caName":format!("dir-{id}")}))
            .collect::<Vec<_>>()
    )
}
fn request_json(call: &Captured) -> Value {
    serde_json::from_str(&decrypt139(&call.body).unwrap()).unwrap()
}
async fn list_scenario(
    responses: Vec<(u16, String)>,
) -> (Result<Vec<Value>, MobileCloudError>, Vec<Captured>) {
    let server = ScriptServer::new(responses);
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        server.client("!!!").list_files("Synthetic", "", "root"),
    )
    .await
    .expect("bounded offline listing");
    (result, server.finish())
}
async fn download_scenario(
    text: String,
) -> (Result<Option<String>, MobileCloudError>, Vec<Captured>) {
    let server = ScriptServer::new(vec![(200, text)]);
    let result = tokio::time::timeout(
        Duration::from_secs(6),
        server.client("!!!").download_url("Synthetic", "file-id"),
    )
    .await
    .expect("bounded offline download");
    (result, server.finish())
}

#[tokio::test]
async fn production_requests_safely_escape_values_and_empty_accounts_use_default() {
    let account = "quote\" backslash\\ $& $` $' $$ {account}\n";
    let link = "share\"\\ $& {pwd} \u{1f600}";
    let pwd = "pwd\"\\\n $` {account}";
    let folder = "folder\"\\ $' {key}";
    let path = "path\"\\\n $& {account}";
    let auth = mcloud::encode_authorization(&format!("pc:{account}:token")).unwrap();
    let wire = r#"{"success":true,"data":{"redrUrl":"https:\/\/cdn.invalid\/a%22b.mp4?x=1\u0026amp;y=2\u0026z=%26"}}"#;
    let server = ScriptServer::new(vec![
        (200, ok_page(Value::Null, Value::Null).to_string()),
        (200, wire.into()),
    ]);
    let client = server.client(&auth);
    let list = tokio::time::timeout(Duration::from_secs(6), client.list_files(link, pwd, folder))
        .await
        .unwrap();
    let download = tokio::time::timeout(Duration::from_secs(6), client.download_url(link, path))
        .await
        .unwrap();
    let calls = server.finish();
    assert_eq!(list.unwrap(), Vec::<Value>::new());
    assert_eq!(
        download.unwrap().as_deref(),
        Some("https://cdn.invalid/a%22b.mp4?x=1&y=2&z=%26")
    );
    assert_eq!(calls.len(), 2);
    assert_eq!(
        request_json(&calls[0]),
        json!({"getOutLinkInfoReq":{"account":account,
        "linkID":link,"passwd":pwd,"caSrt":0,"coSrt":0,"srtDr":1,"bNum":1,"pCaID":folder,"eNum":200},
        "commonAccountInfo":{"account":account,"accountType":1}})
    );
    assert_eq!(
        request_json(&calls[1]),
        json!({"dlFromOutLinkReqV3":{"account":account,"linkID":link,
        "coIDLst":{"item":[path]}},"commonAccountInfo":{"account":account,"accountType":1}})
    );
    assert!(
        serde_json::from_str::<Value>(&list_request_body(account, link, pwd, folder)).is_err(),
        "the old template bug remains frozen, NOT the production behavior"
    );
    assert_ne!(
        normalize_download_response(wire).unwrap(),
        Some("https://cdn.invalid/a%22b.mp4?x=1&y=2&z=%26".into()),
        "the old helper intentionally does not decode HTML ampersands"
    );

    for authorization in [
        String::new(),
        mcloud::encode_authorization("pc::token").unwrap(),
    ] {
        let server = ScriptServer::new(vec![
            (200, ok_page(Value::Null, Value::Null).to_string()),
            (
                200,
                json!({"resultCode":0,"data":{"url":"http://cdn.invalid/file"}}).to_string(),
            ),
        ]);
        let client = server.client(&authorization);
        assert!(
            tokio::time::timeout(Duration::from_secs(6), client.list_files("S", "", "root"))
                .await
                .unwrap()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(6), client.download_url("S", "f"))
                .await
                .unwrap()
                .unwrap()
                .as_deref(),
            Some("http://cdn.invalid/file")
        );
        let calls = server.finish();
        assert_eq!(
            calls.len(),
            2,
            "empty accounts must request, not falsely report success"
        );
        for call in calls {
            assert_eq!(
                request_json(&call)["commonAccountInfo"]["account"],
                "default"
            );
        }
    }
    let server = ScriptServer::new(vec![]);
    assert_eq!(server.client("").download_url("S", "").await.unwrap(), None);
    assert!(
        server.finish().is_empty(),
        "empty file ID is still a legitimate no-request case"
    );
}

#[tokio::test]
async fn strict_json_success_shape_null_lists_and_checked_metadata() {
    let invalid = [
        ("", MobileCloudError::Json),
        ("null", MobileCloudError::ResponseShape),
        ("[]", MobileCloudError::ResponseShape),
        (
            r#"{"success":"true","data":{"caLst":[],"coLst":[]}}"#,
            MobileCloudError::Upstream,
        ),
        (
            r#"{"success":1,"data":{"caLst":[],"coLst":[]}}"#,
            MobileCloudError::Upstream,
        ),
        (
            r#"{"success":false,"resultCode":"1","data":{"caLst":[],"coLst":[]}}"#,
            MobileCloudError::Upstream,
        ),
        (r#"{"success":true}"#, MobileCloudError::ResponseShape),
        (
            r#"{"success":true,"data":null}"#,
            MobileCloudError::ResponseShape,
        ),
        (
            r#"{"success":true,"data":{}}"#,
            MobileCloudError::ResponseShape,
        ),
        (
            r#"{"success":true,"data":{"coLst":[]}}"#,
            MobileCloudError::ResponseShape,
        ),
        (
            r#"{"success":true,"data":{"caLst":[]}}"#,
            MobileCloudError::ResponseShape,
        ),
        (
            r#"{"success":true,"data":{"caLst":[],"coLst":{}}}"#,
            MobileCloudError::ResponseShape,
        ),
        (
            r#"{"success":true,"data":{"caLst":null,"fileList":[]}}"#,
            MobileCloudError::ResponseShape,
        ),
        (
            r#"{"success":true,"data":{"caLst":[null],"coLst":[]}}"#,
            MobileCloudError::ResponseShape,
        ),
        (
            r#"{"success":true,"data":{"caLst":[{"caID":false}],"coLst":[]}}"#,
            MobileCloudError::ResponseShape,
        ),
        (
            r#"{"success":true,"data":{"caLst":[],"coLst":[{"coID":""}]}}"#,
            MobileCloudError::ResponseShape,
        ),
        (
            r#"{"success":true,"data":{"caLst":[],"coLst":[{"coID":"f","coSize":-1}]}}"#,
            MobileCloudError::ResponseShape,
        ),
        (
            r#"{"success":true,"data":{"caLst":[],"coLst":[{"coID":"f","coSize":1.5}]}}"#,
            MobileCloudError::ResponseShape,
        ),
        (
            r#"{"success":true,"data":{"caLst":[],"coLst":[{"coID":"f","coSize":"NaN"}]}}"#,
            MobileCloudError::ResponseShape,
        ),
        (
            r#"{"success":true,"data":{"caLst":[],"coLst":[{"coID":"f","path":5}]}}"#,
            MobileCloudError::ResponseShape,
        ),
        (
            r#"{"success":true,"data":{"caLst":[],"coLst":[],"coLength":true}}"#,
            MobileCloudError::ResponseShape,
        ),
        (
            r#"{"success":true,"data":{"caLst":[],"coLst":[],"caLength":"-1"}}"#,
            MobileCloudError::ResponseShape,
        ),
        (
            r#"{"success":true,"data":{"caLst":[],"coLst":[],"coLength":"20001"}}"#,
            MobileCloudError::Limit,
        ),
    ];
    for (text, expected) in invalid {
        let (result, calls) = list_scenario(vec![(200, text.into())]).await;
        assert_eq!(result, Err(expected), "strict invalid response: {text}");
        assert_eq!(calls.len(), 1);
    }
    let oversized = ok_page(json!([]), files(0, 201));
    assert_eq!(
        list_scenario(vec![(200, oversized.to_string())]).await.0,
        Err(MobileCloudError::ResponseShape)
    );
    for marker in [
        json!({"success":true}),
        json!({"resultCode":"0"}),
        json!({"resultCode":0}),
    ] {
        let mut body = marker;
        body["data"] =
            json!({"caLst":null,"coLst":null,"caLength":"0","coLength":"0","nodNum":"3"});
        let (result, calls) = list_scenario(vec![(200, body.to_string())]).await;
        assert!(result.unwrap().is_empty());
        assert_eq!(calls.len(), 1);
    }
    let mut page = ok_page(
        json!([{"caID":3,"caName":"folder"}]),
        json!([{"coID":"f","coName":"file","coSize":"9007199254740993","coType":"0","isDir":"false","path":"/f"}]),
    );
    page["data"]["nodNum"] = json!(3); // Observed supplier field is NOT a listing total.
    let (result, calls) = list_scenario(vec![(200, page.to_string())]).await;
    let rows = result.unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(rows, json!([{"fileId":"f","fileName":"file","fileSize":"9007199254740993","isFolder":false,"isDir":false,"path":"/f"},
        {"fileId":"3","fileName":"folder","isFolder":true,"isDir":true}]).as_array().unwrap().clone());
    let incomplete = r#"{"success":true,"data":{"caLst":[{"caID":"d","caName":"rescued"}],"coLst":[{"coID":"f"}]"#;
    assert!(
        !normalize_list_response(incomplete).unwrap().is_empty(),
        "legacy rescue remains frozen"
    );
    assert_eq!(
        list_scenario(vec![(200, incomplete.into())]).await.0,
        Err(MobileCloudError::Json),
        "production never promotes partial rescued JSON into a complete directory"
    );
}

#[tokio::test]
async fn observed_page_lengths_continue_full_lists_with_or_without_cursors() {
    for folders in [false, true] {
        for active_cursor in [false, true] {
            for counts in [vec![200, 200, 101], vec![200, 58]] {
                let mut start = 0;
                let mut responses = Vec::new();
                for (page, count) in counts.iter().copied().enumerate() {
                    let mut body = if folders {
                        ok_page(catalogs(start, count), json!([]))
                    } else {
                        ok_page(json!([]), files(start, count))
                    };
                    body["data"]["caLength"] = json!(if folders { count } else { 0 });
                    body["data"]["coLength"] = json!(if folders { 0 } else { count });
                    body["data"]["nodNum"] = json!(2265);
                    if active_cursor && page + 1 < counts.len() {
                        body["data"]["nextPageCursor"] = json!(format!("page-{}", page + 1));
                    }
                    responses.push((200, body.to_string()));
                    start += count;
                }
                let (result, calls) = list_scenario(responses).await;
                let rows = result.unwrap();
                assert_eq!(rows.len(), counts.iter().sum::<usize>());
                assert_eq!(calls.len(), counts.len());
                let prefix = if folders { "d" } else { "f" };
                assert!(
                    rows.iter()
                        .any(|row| row["fileId"] == format!("{prefix}{}", start - 1))
                );
                for (page, call) in calls.iter().enumerate() {
                    let request = request_json(call);
                    assert_eq!(request["getOutLinkInfoReq"]["bNum"], page * 200 + 1);
                    assert_eq!(request["getOutLinkInfoReq"]["eNum"], (page + 1) * 200);
                    if active_cursor && page > 0 {
                        assert_eq!(
                            request["getOutLinkInfoReq"]["nextPageCursor"],
                            format!("page-{page}")
                        );
                    } else {
                        assert!(request["getOutLinkInfoReq"].get("nextPageCursor").is_none());
                    }
                }
            }
        }
    }
}

#[tokio::test]
async fn mixed_catalog_and_content_page_counts_continue_independently() {
    let mut responses = Vec::new();
    let (mut ca_start, mut co_start) = (0, 0);
    for (ca_count, co_count) in [(200, 1), (1, 200), (0, 58)] {
        let mut body = ok_page(catalogs(ca_start, ca_count), files(co_start, co_count));
        body["data"]["caLength"] = json!(ca_count.to_string());
        body["data"]["coLength"] = json!(co_count.to_string());
        responses.push((200, body.to_string()));
        ca_start += ca_count;
        co_start += co_count;
    }
    let (result, calls) = list_scenario(responses).await;
    let rows = result.unwrap();
    assert_eq!(rows.len(), 460);
    assert_eq!(
        rows.iter().filter(|row| row["isFolder"] == true).count(),
        201
    );
    assert_eq!(
        rows.iter().filter(|row| row["isFolder"] == false).count(),
        259
    );
    assert!(rows.iter().any(|row| row["fileId"] == "f258"));
    assert_eq!(calls.len(), 3);
}

#[tokio::test]
async fn page_counts_reject_mismatches_cross_field_conflicts_and_invalid_numbers() {
    for field in ["caLength", "coLength"] {
        for invalid in [
            json!(-1),
            json!(1.5),
            json!("-1"),
            json!("1.5"),
            json!(""),
            json!(" "),
            json!(" 1"),
            json!("NaN"),
            json!(true),
            json!({}),
            json!([]),
            json!("18446744073709551616"),
            json!(18446744073709551616.0),
        ] {
            let mut body = ok_page(catalogs(0, 1), files(0, 1));
            body["data"][field] = invalid.clone();
            let (result, calls) = list_scenario(vec![(200, body.to_string())]).await;
            assert_eq!(
                result,
                Err(MobileCloudError::ResponseShape),
                "{field}={invalid}"
            );
            assert_eq!(calls.len(), 1);
        }
        for declared in [json!(0), json!("2")] {
            let mut body = ok_page(catalogs(0, 1), files(0, 1));
            body["data"]["caLength"] = json!(1);
            body["data"]["coLength"] = json!(1);
            body["data"][field] = declared;
            let (result, calls) = list_scenario(vec![(200, body.to_string())]).await;
            assert_eq!(result, Err(MobileCloudError::Pagination));
            assert_eq!(calls.len(), 1);
        }
        for allowed in [Value::Null, json!(1), json!("001")] {
            let mut body = ok_page(catalogs(0, 1), files(0, 1));
            body["data"][field] = allowed;
            let (result, calls) = list_scenario(vec![(200, body.to_string())]).await;
            assert_eq!(result.unwrap().len(), 2);
            assert_eq!(calls.len(), 1);
        }
    }
    // Swapped declarations have a matching aggregate but conflict with each list.
    let mut body = ok_page(catalogs(0, 1), files(0, 2));
    body["data"]["caLength"] = json!(2);
    body["data"]["coLength"] = json!(1);
    let (result, calls) = list_scenario(vec![(200, body.to_string())]).await;
    assert_eq!(result, Err(MobileCloudError::Pagination));
    assert_eq!(calls.len(), 1);
}

#[tokio::test]
async fn complete_pagination_uses_page_counts_and_inclusive_ranges() {
    let mut pages = Vec::new();
    for (page, (start, count)) in [(0, 200), (200, 200), (400, 1)].into_iter().enumerate() {
        let mut body = ok_page(
            if page < 2 {
                catalogs(page, 1)
            } else {
                Value::Null
            },
            files(start, count),
        );
        body["data"]["caLength"] = json!(if page < 2 { "1" } else { "0" });
        body["data"]["coLength"] = json!(count.to_string());
        pages.push((200, encrypt139_with_iv(&body.to_string(), [7; 16])));
    }
    let (result, calls) = list_scenario(pages).await;
    let rows = result.unwrap();
    assert_eq!(rows.len(), 403);
    assert!(rows.iter().any(|row| row["fileId"] == "f400"));
    assert_eq!(calls.len(), 3);
    for (page, call) in calls.iter().enumerate() {
        let request = request_json(call);
        assert_eq!(request["getOutLinkInfoReq"]["bNum"], page * 200 + 1);
        assert_eq!(request["getOutLinkInfoReq"]["eNum"], (page + 1) * 200);
        assert!(request["getOutLinkInfoReq"].get("nextPageCursor").is_none());
    }
    // The aggregate count may exceed 200 without either individual list being full.
    let (result, calls) = list_scenario(vec![(
        200,
        ok_page(catalogs(0, 150), files(0, 150)).to_string(),
    )])
    .await;
    assert_eq!(result.unwrap().len(), 300);
    assert_eq!(calls.len(), 1, "never infer pagination from ca+co >= 200");
    // A full page count is not a total: an empty tail must confirm completion.
    let mut full = ok_page(json!([]), files(0, 200));
    full["data"]["caLength"] = json!(0);
    full["data"]["coLength"] = json!("200");
    let mut empty = ok_page(json!([]), json!([]));
    empty["data"]["caLength"] = json!(0);
    empty["data"]["coLength"] = json!(0);
    let (result, calls) =
        list_scenario(vec![(200, full.to_string()), (200, empty.to_string())]).await;
    assert_eq!(result.unwrap().len(), 200);
    assert_eq!(calls.len(), 2);
    // Missing page counts: each individual full list still triggers the next range.
    for first in [
        ok_page(json!([]), files(0, 200)),
        ok_page(catalogs(0, 200), json!([])),
    ] {
        let (result, calls) = list_scenario(vec![
            (200, first.to_string()),
            (200, ok_page(json!([]), json!([])).to_string()),
        ])
        .await;
        assert_eq!(result.unwrap().len(), 200);
        assert_eq!(calls.len(), 2);
    }
}

#[tokio::test]
async fn opaque_next_page_cursors_are_echoed_without_json_or_replacement_corruption() {
    let cursors = [
        json!("cursor\"\\\n $& $` {account}"),
        json!({"opaque":["token", 0]}),
    ];
    let mut responses = Vec::new();
    for page in 0..3 {
        let mut body = ok_page(Value::Null, files(page, 1));
        body["data"]["caLength"] = json!(0);
        body["data"]["coLength"] = json!("1");
        body["data"]["nextPageCursor"] = cursors.get(page).cloned().unwrap_or(Value::Null);
        responses.push((200, body.to_string()));
    }
    let (result, calls) = list_scenario(responses).await;
    assert_eq!(result.unwrap().len(), 3);
    assert_eq!(calls.len(), 3);
    assert!(
        request_json(&calls[0])["getOutLinkInfoReq"]
            .get("nextPageCursor")
            .is_none()
    );
    for page in 1..3 {
        let body = request_json(&calls[page]);
        assert_eq!(
            body["getOutLinkInfoReq"]["nextPageCursor"],
            cursors[page - 1]
        );
        assert_eq!(body["getOutLinkInfoReq"]["bNum"], page * 200 + 1);
        assert_eq!(body["getOutLinkInfoReq"]["eNum"], (page + 1) * 200);
    }
    let mut empty_cursor = ok_page(json!([]), files(0, 1));
    empty_cursor["data"]["nextPageCursor"] = json!("");
    let (result, calls) = list_scenario(vec![(200, empty_cursor.to_string())]).await;
    assert_eq!(result.unwrap().len(), 1);
    assert_eq!(calls.len(), 1);
}

#[tokio::test]
async fn repeated_overlapping_or_inconsistent_pages_and_later_errors_never_return_partial_rows() {
    let full = ok_page(json!([]), files(0, 200));
    let mut known = full.clone();
    known["data"]["coLength"] = json!("200");
    let mut changed = ok_page(json!([]), files(200, 1));
    changed["data"]["coLength"] = json!("2");
    let mut too_many = ok_page(json!([]), files(0, 1));
    too_many["data"]["coLength"] = json!("0");
    let mut cursor_a = ok_page(json!([]), files(0, 1));
    cursor_a["data"]["nextPageCursor"] = json!("unchanged");
    let mut cursor_b = ok_page(json!([]), files(1, 1));
    cursor_b["data"]["nextPageCursor"] = json!("unchanged");
    let duplicate_same_page = ok_page(json!([{"caID":"f0"}]), files(0, 1));
    let mut empty_active = ok_page(Value::Null, Value::Null);
    empty_active["data"]["nextPageCursor"] = json!("still-active");
    let cases = vec![
        (
            vec![(200, full.to_string()), (200, full.to_string())],
            MobileCloudError::Pagination,
        ),
        (
            vec![
                (200, full.to_string()),
                (200, ok_page(json!([]), files(199, 2)).to_string()),
            ],
            MobileCloudError::Pagination,
        ),
        (
            vec![(200, duplicate_same_page.to_string())],
            MobileCloudError::Pagination,
        ),
        (
            vec![(200, known.to_string()), (200, changed.to_string())],
            MobileCloudError::Pagination,
        ),
        (
            vec![(200, too_many.to_string())],
            MobileCloudError::Pagination,
        ),
        (
            vec![(200, known.to_string()), (200, empty_active.to_string())],
            MobileCloudError::Pagination,
        ),
        (
            vec![(200, full.to_string()), (503, "unavailable".into())],
            MobileCloudError::Http(503),
        ),
        (
            vec![(200, full.to_string()), (200, "truncated {".into())],
            MobileCloudError::Json,
        ),
        (
            vec![
                (200, full.to_string()),
                (
                    200,
                    json!({"success":false,"data":{"caLst":[],"coLst":[]}}).to_string(),
                ),
            ],
            MobileCloudError::Upstream,
        ),
        (
            vec![(200, cursor_a.to_string()), (200, cursor_b.to_string())],
            MobileCloudError::Pagination,
        ),
    ];
    for (responses, expected) in cases {
        let count = responses.len();
        let (result, calls) = list_scenario(responses).await;
        assert_eq!(result, Err(expected));
        assert_eq!(calls.len(), count);
    }
}

#[tokio::test]
async fn listing_bounds_are_explicit_errors_not_truncated_success() {
    for declared_count in [false, true] {
        let responses = (0..100)
            .map(|page| {
                let mut body = ok_page(json!([]), files(page * 200, 200));
                if declared_count {
                    body["data"]["coLength"] = json!("200");
                }
                (200, body.to_string())
            })
            .collect();
        let (result, calls) = list_scenario(responses).await;
        assert_eq!(
            result,
            Err(MobileCloudError::Limit),
            "memory/page cap must never return a partial list"
        );
        assert!(!calls.is_empty() && calls.len() <= 100);
    }
    let responses = (0..51)
        .map(|page| {
            (
                200,
                ok_page(catalogs(page * 200, 200), files(page * 200, 200)).to_string(),
            )
        })
        .collect();
    let (result, calls) = list_scenario(responses).await;
    assert_eq!(result, Err(MobileCloudError::Limit));
    assert!(
        !calls.is_empty() && calls.len() <= 51,
        "combined lists obey count and memory limits"
    );
    let mut body = ok_page(json!([]), json!([]));
    body["data"]["caLength"] = json!("20001");
    body["data"]["coLength"] = json!(0);
    assert_eq!(
        list_scenario(vec![(200, body.to_string())]).await.0,
        Err(MobileCloudError::Limit)
    );
}

#[tokio::test]
async fn production_downloads_require_complete_json_and_standard_http_urls() {
    for name in ["redrUrl", "downloadUrl", "url"] {
        let mut body = json!({"success":true,"data":{}});
        body["data"][name] = json!(
            "HTTPS://CDN.INVALID/file?one=1&amp;two=2&#38;three=3&#x26;four=4&AMP;five=5&amp;amp;six=6&escaped=%26"
        );
        let (result, calls) = download_scenario(body.to_string()).await;
        assert_eq!(
            result.unwrap().as_deref(),
            Some(
                "HTTPS://CDN.INVALID/file?one=1&two=2&three=3&four=4&five=5&amp;six=6&escaped=%26"
            )
        );
        assert_eq!(calls.len(), 1);
    }
    for invalid in [
        "javascript:alert(1)",
        "data:text/plain,example",
        "file:///tmp/example",
        "ftp://cdn.invalid/file",
        "//cdn.invalid/file",
        "/relative",
        "https:cdn.invalid/file",
        "https:///cdn.invalid/file",
        "https:////cdn.invalid/file",
        "https://",
        "https://user:pass@cdn.invalid/file",
        "https://user@cdn.invalid/file",
        "https://@cdn.invalid/file",
        "https://cdn.invalid/file\\evil",
        " https://cdn.invalid/file",
        "https://cdn.invalid/file\n",
        "https://cdn.invalid:99999/file",
        "https://?x=1",
        "https://#fragment",
    ] {
        let (result, calls) =
            download_scenario(json!({"success":true,"data":{"redrUrl":invalid}}).to_string()).await;
        assert_eq!(
            result,
            Err(MobileCloudError::DownloadUrl),
            "invalid URL: {invalid:?}"
        );
        assert_eq!(calls.len(), 1);
    }
    for (raw, expected) in [
        (
            r#"broken "redrUrl":"https:\/\/cdn.invalid\/raw""#,
            MobileCloudError::Json,
        ),
        ("", MobileCloudError::Json),
        ("null", MobileCloudError::ResponseShape),
        (
            r#"{"success":"false","data":{"url":"https://cdn.invalid/file"}}"#,
            MobileCloudError::Upstream,
        ),
        (
            r#"{"success":true,"data":{"redrUrl":7}}"#,
            MobileCloudError::ResponseShape,
        ),
    ] {
        assert_eq!(download_scenario(raw.into()).await.0, Err(expected));
    }
    for data in [
        json!({}),
        json!({"redrUrl":null}),
        json!({"redrUrl":"","downloadUrl":null}),
    ] {
        assert_eq!(
            download_scenario(json!({"resultCode":"0","data":data}).to_string())
                .await
                .0
                .unwrap(),
            None
        );
    }
}

#[tokio::test]
async fn explicit_failure_markers_override_success_for_lists_and_downloads() {
    for marker in [
        json!({"success":true,"resultCode":403}),
        json!({"success":true,"resultCode":"403"}),
        json!({"success":false,"resultCode":0}),
        json!({"success":false,"resultCode":"0"}),
        json!({"success":true,"resultCode":null}),
    ] {
        let mut body = marker.clone();
        body["data"] = json!({"caLst":[],"coLst":[{"coID":"bait"}]});
        let (result, calls) = list_scenario(vec![(200, body.to_string())]).await;
        assert_eq!(result, Err(MobileCloudError::Upstream), "{marker}");
        assert_eq!(calls.len(), 1);
        body["data"] = json!({"url":"https://cdn.invalid/bait"});
        let (result, calls) = download_scenario(body.to_string()).await;
        assert_eq!(result, Err(MobileCloudError::Upstream), "{marker}");
        assert_eq!(calls.len(), 1);
    }
}

#[tokio::test]
async fn file_list_provenance_overrides_conflicting_subtypes_and_directory_flags() {
    let body = ok_page(
        json!([]),
        json!([
            {"coID":0,"coName":"numeric.mp4","coType":1,"isDir":true,"coSize":0,"contentSize":99},
            {"coID":"unknown","coName":"unknown.mp4","coType":"1","isFolder":true,"isDirectory":"true"}
        ]),
    );
    let (result, calls) = list_scenario(vec![(200, body.to_string())]).await;
    let rows = result.unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(rows[0]["fileId"], "0");
    for row in &rows {
        assert_eq!(row["isFolder"], false);
        assert_eq!(row["isDir"], false);
    }
    assert_eq!(
        rows[0]["fileSize"], 0,
        "explicit zero never falls back to an alias"
    );
    assert!(rows[1]["fileSize"].is_null());
}

#[tokio::test]
async fn signed_download_urls_are_validated_without_reserialization() {
    let raw = "HTTPS://CDN.INVALID:443/a/%2e%2e/b%2fC?sig=%2f%2F&literal=%2526&x=1&amp;y=2";
    let expected = raw.replace("&amp;", "&");
    assert_ne!(
        url::Url::parse(&expected).unwrap().as_str(),
        expected,
        "this regression must actually exercise URL normalization"
    );
    let (result, calls) =
        download_scenario(json!({"success":true,"data":{"url":raw}}).to_string()).await;
    assert_eq!(result.unwrap(), Some(expected));
    assert_eq!(calls.len(), 1);
}

#[tokio::test]
async fn encrypted_production_bodies_do_not_trim_arbitrary_trailing_bytes_into_valid_json() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use cbc::cipher::{BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};
    fn frame(bytes: &[u8]) -> String {
        let iv = [9u8; 16];
        let encrypted =
            cbc::Encryptor::<aes::Aes128>::new(b"PVGDwmcvfs1uV3d1".into(), (&iv).into())
                .encrypt_padded_vec_mut::<Pkcs7>(bytes);
        STANDARD.encode([iv.as_slice(), encrypted.as_slice()].concat())
    }
    let page = ok_page(Value::Null, Value::Null).to_string();
    let mut bytes = page.as_bytes().to_vec();
    bytes.extend(std::iter::repeat_n(b'x', 15));
    bytes.push(16); // A legacy slice drops all 16 bytes without checking padding.
    let wire = frame(&bytes);
    assert_eq!(
        decrypt139(&wire).unwrap(),
        page,
        "manual decode quirk remains frozen"
    );
    assert_eq!(
        list_scenario(vec![(200, wire)]).await.0,
        Err(MobileCloudError::Json)
    );
    let download = json!({"success":true,"data":{"url":"https://cdn.invalid/file"}}).to_string();
    let mut bytes = download.as_bytes().to_vec();
    bytes.extend(std::iter::repeat_n(b'x', 15));
    bytes.push(16);
    assert_eq!(
        download_scenario(frame(&bytes)).await.0,
        Err(MobileCloudError::Json)
    );
    let invalid_utf8 = b"{\"success\":true,\"data\":{\"caLst\":[],\"coLst\":[{\"coID\":\"f\",\"coName\":\"\xff\"}]}}";
    assert_eq!(
        list_scenario(vec![(200, frame(invalid_utf8))]).await.0,
        Err(MobileCloudError::Json)
    );
}

#[tokio::test]
async fn sparse_page_counts_do_not_replace_cursors_and_nod_num_is_not_a_total() {
    for folders in [false, true] {
        for active_cursor in [false, true] {
            let mut first = if folders {
                ok_page(catalogs(0, 1), Value::Null)
            } else {
                ok_page(Value::Null, files(0, 1))
            };
            first["data"][if folders { "caLength" } else { "coLength" }] = json!("1");
            first["data"]["nodNum"] = json!(2265);
            if active_cursor {
                first["data"]["nextPageCursor"] = json!("next");
            }
            // Subsequent pages may omit counts; an active cursor alone continues
            // sparse pages. Without it the first short page is already complete.
            let second = if folders {
                ok_page(catalogs(1, 1), Value::Null)
            } else {
                ok_page(Value::Null, files(1, 1))
            };
            let (result, calls) =
                list_scenario(vec![(200, first.to_string()), (200, second.to_string())]).await;
            let expected = if active_cursor { 2 } else { 1 };
            assert_eq!(result.unwrap().len(), expected);
            assert_eq!(calls.len(), expected, "nodNum=2265 is not a listing total");
            if active_cursor {
                assert_eq!(
                    request_json(&calls[1])["getOutLinkInfoReq"]["nextPageCursor"],
                    "next"
                );
            }
        }
    }
}

async fn wait_flag(flag: &AtomicBool) {
    tokio::time::timeout(Duration::from_secs(6), async {
        while !flag.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("bounded loopback synchronization");
}

#[tokio::test]
async fn shared_limit_is_held_until_full_body_and_cancellation_releases_it() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let started = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let early = Arc::new(AtomicBool::new(false));
    let (server_started, server_release, server_early) =
        (started.clone(), release.clone(), early.clone());
    let server = thread::spawn(move || {
        let Some((mut first, _)) = accept_before(&listener, None) else {
            return 0;
        };
        capture(&mut first);
        let body = ok_page(Value::Null, Value::Null).to_string();
        // Flush all headers and almost the whole body: send().await has completed,
        // but the chunk stream is intentionally not complete.
        write!(
            first,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            &body[..body.len() - 1]
        )
        .unwrap();
        first.flush().unwrap();
        server_started.store(true, Ordering::SeqCst);
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut second = None;
        while !server_release.load(Ordering::SeqCst) && Instant::now() < deadline {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream.set_nonblocking(false).unwrap();
                    capture(&mut stream);
                    server_early.store(true, Ordering::SeqCst);
                    second = Some(stream);
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(2))
                }
                Err(error) => panic!("loopback accept failed: {error}"),
            }
        }
        while !server_release.load(Ordering::SeqCst) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(2));
        }
        // First request is canceled, so an OS-dependent broken pipe is harmless.
        let _ = first.write_all(&body.as_bytes()[body.len() - 1..]);
        let mut second = match second {
            Some(stream) => stream,
            None => {
                let Some((mut stream, _)) = accept_before(&listener, None) else {
                    return 1;
                };
                capture(&mut stream);
                stream
            }
        };
        write!(
            second,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        2
    });
    let limit = settings::UpstreamLimit::new(1);
    let client = MobileCloudClient::new_for_loopback(test_client(), "!!!".into(), address)
        .unwrap()
        .with_limit(limit.clone());
    let first_client = client.clone();
    let first = tokio::spawn(async move { first_client.list_files("S", "", "root").await });
    wait_flag(&started).await;
    let second = tokio::spawn(async move { client.list_files("S", "", "root").await });
    tokio::time::sleep(Duration::from_millis(80)).await;
    let body_held = !early.load(Ordering::SeqCst);
    let budget_held = tokio::time::timeout(Duration::from_millis(20), limit.acquire())
        .await
        .is_err();
    first.abort();
    let canceled = first.await.unwrap_err().is_cancelled();
    release.store(true, Ordering::SeqCst);
    let second_result = tokio::time::timeout(Duration::from_secs(6), second)
        .await
        .unwrap()
        .unwrap();
    let calls = server.join().unwrap();
    assert!(
        body_held,
        "second clone must not send after headers but before the full body"
    );
    assert!(
        budget_held,
        "body read must continue to own the shared permit"
    );
    assert!(canceled);
    assert!(second_result.unwrap().is_empty());
    assert_eq!(calls, 2);
    drop(
        tokio::time::timeout(Duration::from_secs(1), limit.acquire())
            .await
            .unwrap(),
    );
}

#[tokio::test]
async fn shared_limit_permits_are_released_on_http_json_and_shape_errors() {
    for response in [
        (503, "unavailable".into()),
        (200, "broken {".into()),
        (200, json!({"success":true,"data":{}}).to_string()),
    ] {
        let server = ScriptServer::new(vec![
            response,
            (200, ok_page(Value::Null, Value::Null).to_string()),
        ]);
        let limit = settings::UpstreamLimit::new(1);
        let client = server.client("!!!").with_limit(limit.clone());
        assert!(
            tokio::time::timeout(Duration::from_secs(6), client.list_files("S", "", "root"))
                .await
                .unwrap()
                .is_err()
        );
        let rows = tokio::time::timeout(
            Duration::from_secs(6),
            client.clone().list_files("S", "", "root"),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(rows.is_empty());
        assert_eq!(server.finish().len(), 2);
        drop(
            tokio::time::timeout(Duration::from_secs(1), limit.acquire())
                .await
                .unwrap(),
        );
    }
}

#[tokio::test]
async fn test_seam_is_loopback_only_and_caller_disables_redirects() {
    assert!(matches!(
        MobileCloudClient::new_for_loopback(
            test_client(),
            String::new(),
            "192.0.2.1:80".parse().unwrap()
        ),
        Err(MobileCloudError::NonLoopback)
    ));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let Some((mut stream, _)) = accept_before(&listener, None) else {
            return;
        };
        capture(&mut stream);
        // Redirect points at another loopback port, never at a real external host.
        stream.write_all(b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/blocked\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
    });
    let client = MobileCloudClient::new_for_loopback(test_client(), "!!!".into(), address).unwrap();
    assert_eq!(
        client.list_files("S", "", "root").await,
        Err(MobileCloudError::Http(302))
    );
    server.join().unwrap();
    // Errors carry no raw reqwest error, headers, authorization or decoded account.
    assert_eq!(
        MobileCloudError::Transport.to_string(),
        "mobile cloud request failed or timed out"
    );
}
