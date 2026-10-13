//! Offline differential tests: original JS uses strict-memory fetch; Rust HTTP uses
//! numeric loopback only. No real account or production-network request is made.
mod support;
pub use jxpan_rust::compat;
use jxpan_rust::providers::mcloud;
// Include directly so this delegated file can be tested before shared mod.rs wiring.
#[path = "../src/providers/mcloud_client.rs"]
mod mcloud_client;
use mcloud_client::*;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::OnceLock,
    thread,
    time::Duration,
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
fn test_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .unwrap()
}

#[tokio::test]
async fn encrypted_loopback_http_matches_original_requests_and_responses() {
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
            let (mut stream, peer) = listener.accept().unwrap();
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
                .list_files(
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
                .download_url(args[0].as_str().unwrap(), args[1].as_str().unwrap())
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
                    .list_files(
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
                    .download_url(args[0].as_str().unwrap(), args[1].as_str().unwrap())
                    .await
                    .unwrap()
            )
        };
        assert_eq!(actual, vector["expected"]);
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
        let (mut stream, _) = listener.accept().unwrap();
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
