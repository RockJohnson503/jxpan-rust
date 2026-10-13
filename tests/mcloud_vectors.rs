//! Differential corpus evaluated by the pinned current deployment's original methods.
//! JXPAN_LIVE_ORACLE=1 reruns and checks the private original-source oracle.
mod support;

use jxpan_rust::providers::mcloud;
use serde_json::{Value, json};

#[test]
fn current_deployment_node_vm_differential() {
    let root = support::oracle("mcloud", include_str!("fixtures/mcloud.json"));
    assert_eq!(root["deploymentSha256"], mcloud::DEPLOYMENT_SHA256);
    let vectors = root["vectors"].as_array().unwrap();
    assert_eq!(vectors.len(), 4768, "unexpected oracle matrix drift");
    let mut compared = 0;
    let mut failures = Vec::new();
    for (index, vector) in vectors.iter().enumerate() {
        let method = vector["method"].as_str().unwrap();
        let input = &vector["input"];
        let expected = &vector["expected"];
        let mut error = false;
        let actual = match method {
            "extractShareInfo" => json!(mcloud::extract_share_info(input.as_str().unwrap())),
            "extractAccountFromAuth" => json!(mcloud::extract_account_from_auth(input)),
            "buildHeaders" => json!(mcloud::normalize_authorization_header(
                input.as_str().unwrap()
            )),
            "isFolder" => match mcloud::try_is_folder(input) {
                Ok(b) => json!(b),
                Err(_) => {
                    error = true;
                    Value::Null
                }
            },
            "decode" => match mcloud::decode_base64_latin1(input.as_str().unwrap()) {
                Ok(s) => json!(s),
                Err(_) => {
                    error = true;
                    Value::Null
                }
            },
            "encode" => match mcloud::encode_authorization(input.as_str().unwrap()) {
                Ok(s) => json!(s),
                Err(_) => {
                    error = true;
                    Value::Null
                }
            },
            "md5" => json!(mcloud::deployment_md5(input.as_str().unwrap())),
            "pythonQuoteExact" => json!(mcloud::python_quote_exact(input.as_str().unwrap())),
            "generateMcloudSign" => {
                // JSON.stringify comes directly from VM. Never normalize key order
                // or Unicode escapes through serde_json before signing.
                let json_string = expected["json"].as_str();
                let timestamp = expected["timestamp"].as_str().unwrap();
                let nonce = expected["nonce"].as_str().unwrap();
                json!({ "json":expected["json"], "timestamp":timestamp, "nonce":nonce,
                    "signHeader":mcloud::generate_mcloud_sign(json_string,timestamp,nonce) })
            }
            _ => panic!("unknown oracle method {method}"),
        };
        compared += 1;
        if actual != *expected || error != vector["error"].as_bool().unwrap() {
            // All inputs are synthetic; never add real credentials to this corpus.
            failures.push(format!("vector {index} {method}, input={input}, expected={expected}, actual={actual}, expected_error={}, error={error}", vector["error"]));
        }
    }
    assert!(
        failures.is_empty(),
        "{} / {compared} differences:\n{}",
        failures.len(),
        failures
            .iter()
            .take(15)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
    eprintln!(
        "MCLOUD: {compared} live Node VM/Rust vectors passed; deployment {}",
        mcloud::DEPLOYMENT_SHA256
    );
}
