mod support;
use jxpan_rust::providers::{mcloud_auth as auth, mcloud_auth_rsa as rsa};
use serde_json::{Value, json};
fn vectors() -> Value {
    support::oracle("mcloud-auth", include_str!("fixtures/mcloud-auth.json"))
}
fn bytes(v: &Value) -> Vec<u8> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap() as u8)
        .collect()
}
fn capture(r: &auth::Request) -> Value {
    json!({"url":r.url,"method":r.method,"headers":r.headers,"body":r.body})
}
fn session(v: &Value) -> auth::Session {
    auth::Session {
        device_id: v["deviceId"].as_str().unwrap().into(),
        sid: v["sid"].as_str().unwrap().into(),
        aes_key: v["aesKey"].as_str().unwrap().into(),
        mcloud_skey: v["mcloudSkey"].as_str().unwrap().into(),
    }
}
fn hex(v: &[u8]) -> String {
    let mut s = v.iter().map(|b| format!("{b:02x}")).collect::<String>();
    while s.len() > 1 && s.starts_with('0') {
        s.remove(0);
    }
    s
}
#[test]
fn aes_matches_frozen_webcrypto() {
    for v in vectors()["ecb"].as_array().unwrap() {
        assert_eq!(
            auth::aes_ecb_encrypt(
                v["key"].as_str().unwrap().as_bytes(),
                v["text"].as_str().unwrap().as_bytes()
            )
            .unwrap(),
            v["expected"].as_str().unwrap()
        );
    }
    assert!(auth::aes_ecb_encrypt(b"short", b"x").is_err());
}
#[test]
fn rsa_matches_frozen_bignum_and_der_quirks() {
    let vs = vectors();
    for (i, v) in vs["rsa"].as_array().unwrap().iter().enumerate() {
        let actual = rsa::rsa_encrypt(
            &bytes(&v["n"]),
            &bytes(&v["e"]),
            v["message"].as_str().unwrap().as_bytes(),
            &bytes(&v["padding"]),
        )
        .unwrap();
        assert!(actual == v["expected"].as_str().unwrap(), "RSA vector {i}");
    }
    for v in vs["parse"].as_array().unwrap() {
        let (n, e) = rsa::parse_deployed_public_key(v["encoded"].as_str().unwrap()).unwrap();
        assert_eq!(hex(&n), v["n"].as_str().unwrap());
        assert_eq!(hex(&e), v["e"].as_str().unwrap());
    }
    assert!(rsa::rsa_encrypt(&[3], &[3], b"too long", &[]).is_err());
}
#[test]
fn qr_request_session_and_response_match_original_route() {
    let vs = vectors();
    let uuids = vs["uuids"].as_array().unwrap();
    let mut random = [0; 16];
    random.copy_from_slice(&bytes(&vs["random"]));
    let nonce = auth::nonce(&random);
    for (i, v) in vs["qrcode"].as_array().unwrap().iter().enumerate() {
        let mut s = auth::Session::from_uuids(
            uuids[0].as_str().unwrap(),
            uuids[1].as_str().unwrap(),
            uuids[2].as_str().unwrap(),
        );
        let request = auth::key_request(&s, vs["timestamp"].as_str().unwrap(), &nonce);
        assert!(capture(&request) == v["calls"][0], "QR request {i}");
        if let Some(error) = v["error"].as_str() {
            assert_eq!(auth::qr_failure(error), v["expected"]);
            continue;
        }
        let n = if i == 5 { 64 - 19 } else { 0 };
        let padding: Vec<u8> = (0..n).map(|i| ((i * 17) % 256) as u8).collect();
        if let Err(error) = auth::finish_key(&mut s, &v["response"], &padding) {
            assert_eq!(auth::qr_failure(error), v["expected"]);
            continue;
        }
        assert_eq!(s.stored_value(), v["writes"][0]["value"]);
        assert_eq!(s.qr_response(), v["expected"]);
        assert_eq!(v["writes"][0]["options"], json!({"expirationTtl":300}));
    }
}
#[test]
fn polling_matches_original_route() {
    let vs = vectors();
    let s = session(&vs["qrcode"].as_array().unwrap().last().unwrap()["writes"][0]["value"]);
    let mut random = [0; 16];
    random.copy_from_slice(&bytes(&vs["random"]));
    let request =
        auth::poll_request(&s, vs["timestamp"].as_str().unwrap(), &auth::nonce(&random)).unwrap();
    for (i, v) in vs["poll"].as_array().unwrap().iter().enumerate() {
        let opt = &v["options"];
        let actual = if opt["missing"] == true {
            auth::poll_missing_session()
        } else if opt["noStorage"] == true {
            auth::poll_missing_storage()
        } else if let Some(error) = opt["error"].as_str().or(opt["parseError"].as_str()) {
            auth::poll_failure(error)
        } else {
            auth::poll_response(&v["response"])
        };
        assert!(
            actual == v["expected"],
            "poll response {i}: actual={actual}, expected={}",
            v["expected"]
        );
        if !v["calls"].as_array().unwrap().is_empty() {
            assert!(capture(&request) == v["calls"][0], "poll request {i}");
        }
    }
}
#[test]
fn refresh_window_backoff_xml_and_write_intents_match_original() {
    let vs = vectors();
    for (i, v) in vs["refresh"].as_array().unwrap().iter().enumerate() {
        let plan = auth::prepare_refresh(Some(&v["stored"]), vs["now"].as_i64().unwrap());
        let actual_calls: Vec<_> = plan.request.iter().map(capture).collect();
        assert!(json!(actual_calls) == v["calls"], "refresh request {i}");
        let response = if v["error"].is_string() {
            None
        } else {
            Some((
                v["response"]["ok"].as_bool().unwrap_or(true),
                v["response"]["text"].as_str().unwrap_or(""),
            ))
        };
        let actual = auth::finish_refresh(plan, response);
        assert_eq!(
            actual.refreshed,
            v["expected"].as_bool().unwrap(),
            "refresh outcome {i}"
        );
        assert_eq!(
            actual
                .authorization
                .as_deref()
                .unwrap_or("configured-sentinel"),
            v["authorization"].as_str().unwrap(),
            "refresh authorization {i}"
        );
        let expected_write = v["writes"]
            .as_array()
            .unwrap()
            .first()
            .map(|v| v["value"].clone());
        assert!(actual.write_info == expected_write, "refresh write {i}");
    }
}
