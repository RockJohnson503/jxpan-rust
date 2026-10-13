//! Differential corpus evaluated by the pinned current deployment's original methods.
//! JXPAN_LIVE_ORACLE=1 reruns and checks the private original-source oracle.
mod support;

use jxpan_rust::providers::cloud189::*;
use serde_json::{Value, json};

fn text(value: &Value) -> &str {
    value.as_str().expect("oracle string")
}

#[test]
fn deployed_node_vm_vectors_match_rust() {
    let oracle = support::oracle("cloud189", include_str!("fixtures/cloud189.json"));
    assert_eq!(
        text(&oracle["sourceHash"]),
        "47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118"
    );
    assert_eq!(
        text(&oracle["classHash"]),
        "29689a2973e07b11e27280ef3eaec0a4653d73267e81f9d15faf2bedccff46b2"
    );
    let shares = oracle["shares"].as_array().unwrap();
    assert_eq!(shares.len(), 49);
    for vector in shares {
        let input = text(&vector["input"]);
        let actual = extract_share_info(input).map(|info| {
            json!({ "shareCode": info.share_code, "shareUrl": info.share_url,
                "matchedPattern": info.matched_pattern })
        });
        assert_eq!(
            actual.unwrap_or(Value::Null),
            vector["expected"],
            "share input: {input:?}"
        );
    }
    let hashes = oracle["hashes"].as_array().unwrap();
    assert_eq!(hashes.len(), 135);
    assert!(hashes.iter().any(|v| text(&v["md5"]).starts_with('0')));
    for vector in hashes {
        let input = text(&vector["input"]);
        assert_eq!(md5(input), text(&vector["md5"]), "MD5 input: {input:?}");
        assert_eq!(
            java_md5(input),
            text(&vector["javaMd5"]),
            "javaMd5 input: {input:?}"
        );
    }
    let hmacs = oracle["hmacs"].as_array().unwrap();
    assert_eq!(hmacs.len(), 5);
    for vector in hmacs {
        let actual = hmac_sha1(text(&vector["key"]), text(&vector["message"]));
        if vector["rejected"] == true {
            assert_eq!(actual, Err(SignatureError::EmptyKey));
        } else {
            assert_eq!(actual.unwrap(), text(&vector["output"]));
        }
    }

    // Original REQUEST methods provide the signing strings, not handwritten JS
    // copies of those expressions. Their fetch is a strict in-memory stub.
    let fixed = &oracle["fixed"];
    let token = text(&fixed["token"]);
    let timestamp = text(&fixed["timestamp"]);
    let file_id = text(&fixed["fileId"]);
    let share_id = text(&fixed["shareId"]);
    let session_key = text(&fixed["sessionKey"]);
    let date = text(&fixed["date"]);
    let secret = text(&fixed["secret"]);
    let signing_texts = [
        open_download_signing_text(token, timestamp, file_id, share_id),
        app_signing_text(session_key, "GET", "/getFileDownloadUrl.action", date),
        app_signing_text(session_key, "GET", "/getFileDownloadUrl.action", date),
        open_list_signing_text(token, timestamp, file_id, share_id),
        app_signing_text(session_key, "GET", "/open/share/listShareDir.action", date),
        web_signing_text(timestamp),
    ];
    let calls = oracle["signingCalls"].as_array().unwrap();
    let requests = oracle["requests"].as_array().unwrap();
    assert_eq!(calls.len(), signing_texts.len());
    assert_eq!(requests.len(), signing_texts.len());
    for (index, expected_text) in signing_texts.iter().enumerate() {
        let call = &calls[index];
        let algorithm = text(&call["algorithm"]);
        let (original_text, actual_signature) = match algorithm {
            "javaMd5" => (text(&call["args"][0]), java_md5(expected_text)),
            "md5" => (text(&call["args"][0]), md5(expected_text)),
            "hmacSha1" => {
                assert_eq!(text(&call["args"][0]), secret);
                (
                    text(&call["args"][1]),
                    hmac_sha1(secret, expected_text).unwrap(),
                )
            }
            _ => panic!("unexpected algorithm"),
        };
        assert_eq!(
            expected_text, original_text,
            "original signing format #{index}"
        );
        assert_eq!(
            actual_signature,
            text(&call["output"]),
            "signature #{index}"
        );
        let request = &requests[index];
        assert_eq!(text(&request["method"]), "GET");
        // Listing carries Open API signature in query; others carry headers.
        let transmitted = if index == 3 {
            url::Url::parse(text(&request["url"]))
                .unwrap()
                .query_pairs()
                .find(|(key, _)| key == "signature")
                .unwrap()
                .1
                .into_owned()
        } else {
            let headers = &request["headers"];
            text(if algorithm == "hmacSha1" {
                &headers["Signature"]
            } else {
                &headers["signature"]
            })
            .to_owned()
        };
        assert_eq!(transmitted, actual_signature, "wire signature #{index}");
    }
    assert!(text(&requests[1]["url"]).contains("dt=1"));
    assert!(text(&requests[2]["url"]).contains("dt=3"));
}
