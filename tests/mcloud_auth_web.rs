//! Independent web-login regressions. No provider traffic, credentials or fixture mutation.
use aes::cipher::{BlockDecrypt, KeyInit};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use cbc::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};
use jxpan_rust::providers::{mcloud, mcloud_auth as ma, mcloud_auth_rsa as rsa};
use serde_json::{Value, json};

fn tlv(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    match body.len() {
        0..128 => out.push(body.len() as u8),
        128..256 => out.extend([0x81, body.len() as u8]),
        _ => out.extend([0x82, (body.len() >> 8) as u8, body.len() as u8]),
    }
    out.extend(body);
    out
}

// Synthetic public modulus, deliberately not a supplier key or an RSA key pair.
// Parsing/encryption need only the public components; no private material exists.
fn spki(width: usize) -> (Vec<u8>, Vec<u8>) {
    let mut modulus = vec![0xa5; width];
    modulus[width - 1] = 0xd3;
    let mut signed = vec![0];
    signed.extend(&modulus);
    let mut key = tlv(2, &signed);
    key.extend(tlv(2, &[1, 0, 1]));
    let mut bits = vec![0];
    bits.extend(tlv(0x30, &key));
    let mut body = vec![
        0x30, 0x0d, 6, 9, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 1, 1, 1, 5, 0,
    ];
    body.extend(tlv(3, &bits));
    (tlv(0x30, &body), modulus)
}

fn key_response() -> Value {
    json!({"success":true,"data":{"publicKey":STANDARD.encode(spki(256).0)}})
}

fn session(seed: &str) -> ma::Session {
    ma::Session::from_uuids(
        &format!("{seed}0112233-4455-4677-8899-aabbccddeeff"),
        &format!("{seed}0213243-5465-4767-8899-aabbccddeeff"),
        &format!("{seed}0314253-6475-4867-8899-aabbccddeeff"),
    )
}

fn login(code: Value, success: Value) -> Value {
    json!({"success":success,"data":{"result":{"resultCode":code}}})
}

#[test]
fn frozen_synthetic_response_is_not_mistaken_for_a_valid_web_spki() {
    let vectors: Value = serde_json::from_str(include_str!("fixtures/mcloud-auth.json")).unwrap();
    let response = vectors["qrcode"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| {
            v["writes"][0]["value"]["mcloudSkey"]
                .as_str()
                .is_some_and(|s| !s.is_empty())
        })
        .unwrap()["response"]
        .clone();
    // Frozen vectors test the old decoder's quirks, not standards-valid ASN.1.
    assert!(ma::web_key(&response).is_err());
}

#[test]
fn web_spki_reads_real_tag_boundaries_and_long_form_integer_lengths() {
    for width in [64, 128, 256, 384, 512] {
        let (der, modulus) = spki(width);
        let (n, e) = rsa::parse_web_public_key(&STANDARD.encode(&der)).unwrap();
        assert!(n == modulus, "modulus bytes must not be offset/truncated");
        assert!(
            e == [1, 0, 1],
            "public exponent must follow the full INTEGER"
        );
    }
    // The oracle kernel is intentionally not fixed in place. Its blind tag walk
    // and last-length-octet INTEGER slicing do not recover a proper 2048-bit key.
    // This independent regression must not change the frozen compatibility data.
    let (der, modulus) = spki(256);
    let legacy = rsa::parse_deployed_public_key(&STANDARD.encode(der));
    assert!(match legacy {
        Ok((n, e)) => n != modulus || e != [1, 0, 1],
        Err(_) => true,
    });
}

#[test]
fn web_spki_rejects_truncation_wrong_algorithm_negative_integer_and_trailing_bytes() {
    let (der, _) = spki(256);
    for length in 0..der.len() {
        assert!(rsa::parse_web_public_key(&STANDARD.encode(&der[..length])).is_err());
    }
    let mut wrong_oid = der.clone();
    let oid = wrong_oid
        .windows(9)
        .position(|b| b == [0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 1, 1, 1])
        .unwrap();
    wrong_oid[oid + 8] = 2;
    assert!(rsa::parse_web_public_key(&STANDARD.encode(wrong_oid)).is_err());
    let mut trailing = der.clone();
    trailing.push(0);
    assert!(rsa::parse_web_public_key(&STANDARD.encode(trailing)).is_err());
    let mut negative = der.clone();
    let integer = negative
        .windows(5)
        .position(|b| b == [2, 0x82, 1, 1, 0])
        .unwrap();
    negative[integer + 4] = 0x80;
    assert!(rsa::parse_web_public_key(&STANDARD.encode(negative)).is_err());
    assert!(rsa::parse_web_public_key(&"A".repeat(4096)).is_err());
    assert!(rsa::parse_web_public_key("not-base64").is_err());
}

#[test]
fn web_key_requires_success_and_a_valid_key_and_clears_old_wrapped_key_on_failure() {
    for response in [
        Value::Null,
        json!({}),
        json!({"success":false,"data":{"publicKey":key_response()["data"]["publicKey"]}}),
        json!({"success":"true","data":{"publicKey":key_response()["data"]["publicKey"]}}),
        json!({"success":true}),
        json!({"success":true,"data":{"publicKey":""}}),
        json!({"success":true,"data":{"publicKey":42}}),
        json!({"success":true,"data":{"publicKey":"bad-key"}}),
    ] {
        let mut s = session("a");
        s.mcloud_skey = "synthetic-stale-wrapped-key".into();
        assert!(ma::finish_web_key(&mut s, &response, &[]).is_err());
        assert!(s.mcloud_skey.is_empty());
    }
    let mut s = session("a");
    ma::finish_web_key(&mut s, &key_response(), &vec![7; 256 - 16 - 3]).unwrap();
    assert_eq!(STANDARD.decode(&s.mcloud_skey).unwrap().len(), 256);
    assert!(ma::finish_web_key(&mut s, &key_response(), &[7; 8]).is_err());
    assert!(s.mcloud_skey.is_empty());
}

#[test]
fn web_poll_classifies_only_explicit_web_states_and_rejects_upstream_errors() {
    for (code, expected) in [
        ("200059541", "waiting"),
        ("200059548", "scanned"),
        ("200059542", "expired"),
        ("200059549", "expired"),
    ] {
        // Known polling states may be carried in a success:false envelope.
        for success in [true, false] {
            let result = ma::poll_web_response(&login(json!(code), json!(success))).unwrap();
            assert_eq!(result["data"]["status"], expected);
        }
    }
    for response in [
        Value::Null,
        json!({}),
        json!({"success":false,"message":"synthetic-upstream-error"}),
        login(json!("unknown-error"), json!(false)),
        login(json!("unknown-error"), json!(true)),
        login(json!(0), json!(true)),
        login(json!("0"), json!(false)),
        login(json!("0"), json!(true)),
        login(json!("200059541"), json!("true")),
    ] {
        assert!(ma::poll_web_response(&response).is_err());
    }
}

#[test]
fn web_confirmation_validates_account_and_preserves_pc_token_and_refresh_compatibility() {
    let mut response = login(json!("0"), json!(true));
    response["data"]["encryptAccount"] = json!(STANDARD.encode("synthetic-account"));
    response["data"]["authToken"] = json!("synthetic|token|with|9999999999999:colon");
    let result = ma::poll_web_response(&response).unwrap();
    assert_eq!(result["data"]["status"], "confirmed");
    let auth = result["data"]["authorization"].as_str().unwrap();
    let decoded = mcloud::decode_base64_latin1(auth.strip_prefix("Basic ").unwrap()).unwrap();
    assert!(decoded == "pc:synthetic-account:synthetic|token|with|9999999999999:colon");
    let plan = ma::prepare_refresh(Some(&json!({"authorization":auth})), 1_000_000);
    assert!(plan.authorization.as_deref() == Some(auth));
    assert!(plan.request.is_none());
    assert_eq!(plan.info["jxpan_auto_refresh_managed"], true);

    response["data"]["encryptAccount"] = json!("");
    assert!(ma::poll_web_response(&response).is_err());
    response["data"]["simplifyAccount"] = json!("synthetic-account");
    response["data"]["token"] = response["data"]["authToken"].take();
    assert!(ma::poll_web_response(&response).is_ok());
    response["data"]["simplifyAccount"] = json!("masked***account");
    assert!(ma::poll_web_response(&response).is_err());
    response["data"]["simplifyAccount"] = json!("synthetic-account");
    response["data"]["token"] = json!("synthetic\ninvalid");
    assert!(ma::poll_web_response(&response).is_err());
}

fn decrypt_poll(s: &ma::Session, request: &ma::Request) -> Value {
    let body: Value = serde_json::from_str(&request.body).unwrap();
    assert_eq!(body["clientId"], "10701");
    assert_eq!(body["returnToken"], true);
    let mut blocks = STANDARD
        .decode(body["encryptMsg"].as_str().unwrap())
        .unwrap();
    let aes = aes::Aes128::new_from_slice(s.aes_key.as_bytes()).unwrap();
    for block in blocks.chunks_exact_mut(16) {
        aes.decrypt_block(aes::cipher::generic_array::GenericArray::from_mut_slice(
            block,
        ));
    }
    let padding = *blocks.last().unwrap() as usize;
    assert!(
        blocks[blocks.len() - padding..]
            .iter()
            .all(|b| *b as usize == padding)
    );
    blocks.truncate(blocks.len() - padding);
    serde_json::from_slice(&blocks).unwrap()
}

#[test]
fn independent_web_sessions_keep_sid_key_device_and_request_encryption_isolated() {
    let mut a = session("a");
    let mut b = session("b");
    ma::finish_web_key(&mut a, &key_response(), &vec![3; 237]).unwrap();
    ma::finish_web_key(&mut b, &key_response(), &vec![5; 237]).unwrap();
    let request_a = ma::poll_request(&a, "2026-05-14 12:34:56", "synthetic-nonce-a").unwrap();
    let request_b = ma::poll_request(&b, "2026-05-14 12:34:56", "synthetic-nonce-b").unwrap();
    assert!(a.sid != b.sid && a.aes_key != b.aes_key && a.device_id != b.device_id);
    assert!(request_a.body != request_b.body);
    assert!(request_a.headers["mcloud-skey"] != request_b.headers["mcloud-skey"]);
    for (s, r) in [(&a, &request_a), (&b, &request_b)] {
        let inner = decrypt_poll(s, r);
        assert!(inner["dycPwd"].as_str() == Some(&s.sid));
        assert_eq!(inner["loginStyle"], "QRCode");
        assert_eq!(inner["clientEnv"], "3");
        assert_eq!(inner["setCookie"], 0);
        let qr = s.qr_response();
        assert_eq!(qr["data"]["expires_in"], 300);
        assert_eq!(qr["data"]["interval"], 3);
        assert!(
            qr["data"]["qr_url"]
                .as_str()
                .unwrap()
                .contains(&format!("sID={}&dID={}", s.sid, s.device_id))
        );
    }
}

// Independently generated with Node.js node:crypto (standard MD5/SHA1,
// createCipheriv aes-256-cbc/aes-128-ecb and default PKCS7), not the oracle.
// Synthetic account/session only; keys below are public website protocol constants.
const WEB_KEY: &[u8; 32] = b"UqEZkrjCKfa02pP6jntzFmkzOz86zHUC";
const DATA_KEY: &[u8; 16] = b"qPqDw263XgFgL3u8";
const WEB_TIME: &str = "2026-05-14 12:34:56";
const WEB_NONCE: &str = "synthetic-nonce-a";
const WEB_IV: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
const WEB_PLAINTEXT: &str = r#"{"msisdn":"","random":"","dycpwd":"a021324354654767","cpid":292,"clienttype":670,"version":"mCloud_4.3.0_536","pintype":21,"secinfo":"9793F06C1A44F870B15610A286A337398D77874D","loginMode":"0","extInfo":{}}"#;
const WEB_BODY: &str = "AAECAwQFBgcICQoLDA0OD9tW81bcq/mNoNaTPKwKkWvKcESCdhJSqXH0OM0Lah6CKZOIuQg8p5C27zLVDLL5JKS9ai77jVf1gUxGHPknSYbxg6BP1kWZGLtSNPtu6Bj1Rzuebdan2VaLXFWLuF+WgJzsNuxnIx5dSOkwK2BWN9oBuZHNJFD2/6d6KJ2X5+G0PoWhPrZyj6f4oPKoFYaitM3Rozo0vv/Jvmk+18j7gWYuQzVfyQSfJsZcDEduBVjTN4ypiki9lTzjshr6ikC+5tRNETEnPqi/hSp8A7RmA5w=";
const ECB_DATA: &str = "67cfa4530104253953e1ebf982f24b0542b369de1fab9f1737432703a50f2cc476740c136538d4444130296aa249310a707d311716004467dcb871a8853e51d0";
const CBC_RESPONSE: &str = "AAECAwQFBgcICQoLDA0OD8lyzP1hSwwpIgn/L73rXopJwDLbqkIKI7reVQ2yGh5Kd9E8pIxvbE7pjmOlqd5zDPStwQefC2hjaRK0SJOuV0jBeN7tdAZD1PNfq6l/7I/XngZ8uqZHTHsl9dSypw8prLfVrc1EwJxuSH2JbWJhrDyBNq4GI1VGsKvH2ZWYhOiHJeHhl5hOpAx7EckLUSfXC9un8YJ5hWUgxiGe56U/0Qo=";

fn web_session() -> ma::Session {
    let mut s = session("a");
    ma::finish_web_key(&mut s, &key_response(), &vec![7; 237]).unwrap();
    s
}

fn decrypt_web_body(request: &ma::Request) -> String {
    let encoded: String = serde_json::from_str(&request.body).unwrap();
    let bytes = STANDARD.decode(encoded).unwrap();
    let plaintext = cbc::Decryptor::<aes::Aes256>::new(WEB_KEY.into(), (&bytes[..16]).into())
        .decrypt_padded_vec_mut::<Pkcs7>(&bytes[16..])
        .unwrap();
    String::from_utf8(plaintext).unwrap()
}

fn wrap_web_bytes(plaintext: &[u8]) -> String {
    let ciphertext = cbc::Encryptor::<aes::Aes256>::new(WEB_KEY.into(), (&WEB_IV).into())
        .encrypt_padded_vec_mut::<Pkcs7>(plaintext);
    let mut bytes = WEB_IV.to_vec();
    bytes.extend(ciphertext);
    STANDARD.encode(bytes)
}

fn wrap_web(response: &Value) -> String {
    wrap_web_bytes(response.to_string().as_bytes())
}

fn encrypt_data(data: &Value) -> String {
    let encoded = ma::aes_ecb_encrypt(DATA_KEY, data.to_string().as_bytes()).unwrap();
    STANDARD
        .decode(encoded)
        .unwrap()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn thirdlogin(code: Value) -> Value {
    json!({"code":code,"data":{"account":"synthetic-account","token":"synthetic|token:colon"}})
}

#[test]
fn thirdlogin_standard_sign_and_cbc_body_match_independent_node_crypto_vectors() {
    let s = web_session();
    let key = ma::web_key_request(&s, WEB_TIME, WEB_NONCE);
    assert_eq!(key.url, ma::KEY_URL);
    assert_eq!(key.body, r#"{"clientCode":"10701","type":1}"#);
    assert_eq!(
        key.headers["mcloud-sign"],
        "2026-05-14 12:34:56,synthetic-nonce-a,05E082C0E8C08D0D50F7750396D5A706"
    );
    assert!(!key.headers.contains_key("mcloud-skey"));
    let request = ma::web_poll_request(&s, WEB_TIME, WEB_NONCE, &WEB_IV).unwrap();
    assert_eq!(request.url, ma::THIRD_LOGIN_URL);
    assert_eq!(request.method, "POST");
    assert_eq!(request.body, serde_json::to_string(WEB_BODY).unwrap());
    assert_eq!(decrypt_web_body(&request), WEB_PLAINTEXT);
    assert_eq!(
        request.headers["mcloud-sign"],
        "2026-05-14 12:34:56,synthetic-nonce-a,96247A711242BC9957ADB159F4E75AAD"
    );
    // Current web signing is standard MD5; the oracle's nonstandard digest stays frozen.
    assert_ne!(
        request.headers["mcloud-sign"],
        mcloud::generate_mcloud_sign(Some(WEB_PLAINTEXT), WEB_TIME, WEB_NONCE)
    );
    assert_eq!(
        ma::key_request(&s, WEB_TIME, WEB_NONCE).body,
        r#"{"clientCode":"10701","type":"1"}"#
    );
    assert_eq!(
        ma::poll_request(&s, WEB_TIME, WEB_NONCE).unwrap().url,
        ma::LOGIN_URL
    );
    // Node encodeURIComponent vector covers its special ASCII punctuation and
    // UTF-8 percent encoding, not Python quote or a Latin-1 approximation.
    let mut punctuation = s.clone();
    punctuation.sid = "qr!'()*中".into();
    let special = ma::web_poll_request(&punctuation, WEB_TIME, WEB_NONCE, &WEB_IV).unwrap();
    assert_eq!(
        special.headers["mcloud-sign"],
        "2026-05-14 12:34:56,synthetic-nonce-a,A59378362A5281A1E3EB7E069C2B7B0C"
    );
}

#[test]
fn thirdlogin_headers_are_web_specific_and_never_reuse_account_authorization() {
    let s = web_session();
    let poll = ma::web_poll_request(&s, WEB_TIME, WEB_NONCE, &WEB_IV).unwrap();
    let key = ma::web_key_request(&s, WEB_TIME, WEB_NONCE);
    for request in [&poll, &key] {
        for (name, expected) in [
            ("caller", "web"),
            ("mcloud-client", "10701"),
            ("mcloud-channel", "1000101"),
            ("mcloud-route", "001"),
            ("mcloud-version", "7.18.1"),
            ("x-huawei-channelSrc", "10000034"),
            ("x-inner-ntwk", "2"),
            ("x-m4c-caller", "PC"),
            ("x-m4c-src", "10002"),
            ("INNER-HCY-ROUTER-HTTPS", "1"),
            ("CMS-DEVICE", "default"),
            ("x-yun-api-version", "v1"),
            ("x-yun-app-channel", "10000034"),
            ("x-yun-module-type", "100"),
        ] {
            assert_eq!(request.headers[name], expected);
        }
        let device = format!("||9|7.18.1|edge||{}||windows 10||zh-CN|||", s.device_id);
        assert_eq!(request.headers["x-deviceinfo"], device);
        assert_eq!(
            request.headers["x-yun-client-info"],
            format!("{device}ZWRnZQ==||")
        );
        assert!(request.headers.keys().all(|name| {
            !["authorization", "cookie"].contains(&name.to_ascii_lowercase().as_str())
        }));
        assert!(!request.body.contains(&s.mcloud_skey));
        assert!(!request.body.contains(&s.aes_key));
    }
    // This flag enables CBC at the gateway; plaintext key bootstrap must not set it.
    assert!(!key.headers.contains_key("hcy-cool-flag"));
    assert!(!key.headers.contains_key("mcloud-skey"));
    assert_eq!(poll.headers["hcy-cool-flag"], "1");
    assert!(poll.headers["mcloud-skey"] == s.mcloud_skey);
}

#[test]
fn thirdlogin_fixed_outer_cbc_and_hex_ecb_response_produce_refresh_compatible_auth() {
    let expected = "pc:synthetic-account:synthetic|token:colon";
    for input in [
        CBC_RESPONSE.to_owned(),
        serde_json::to_string(CBC_RESPONSE).unwrap(),
        json!({"code":"0000","data":ECB_DATA}).to_string(),
        json!({"code":"0000","data":ECB_DATA.to_ascii_uppercase()}).to_string(),
    ] {
        let result = ma::web_poll_response_text(&input).unwrap();
        assert_eq!(result["data"]["status"], "confirmed");
        assert_eq!(result["data"]["phone"], "synthetic-account");
        let auth = result["data"]["authorization"].as_str().unwrap();
        assert!(mcloud::decode_authorization(auth).unwrap() == expected);
        let plan = ma::prepare_refresh(Some(&json!({"authorization":auth})), 1_000_000);
        assert!(plan.authorization.as_deref() == Some(auth));
        assert!(plan.request.is_none());
        assert!(!result.to_string().contains(ECB_DATA));
    }
}

#[test]
fn thirdlogin_all_explicit_poll_states_and_code_boundaries_are_fail_closed() {
    for (code, expected) in [
        ("200059541", "waiting"),
        ("200059548", "scanned"),
        ("200059542", "expired"),
        ("200059549", "expired"),
    ] {
        for response in [
            json!({"code":code}),
            json!({"code":code,"success":false}),
            json!({"success":false,"data":{"result":{"resultCode":code}}}),
        ] {
            let result = ma::web_poll_response_text(&wrap_web(&response)).unwrap();
            assert_eq!(result["data"]["status"], expected);
            assert!(result["data"].get("authorization").is_none());
        }
    }
    for code in ["0", "0000"] {
        for success in [None, Some(true)] {
            let mut response = thirdlogin(json!(code));
            if let Some(success) = success {
                response["success"] = json!(success);
            }
            assert!(ma::web_poll_response_text(&wrap_web(&response)).is_ok());
            response.as_object_mut().unwrap().remove("code");
            response["data"]["result"] = json!({"resultCode":code});
            assert!(ma::web_poll_response_text(&wrap_web(&response)).is_ok());
        }
    }
    for code in [
        json!(0),
        json!(200),
        Value::Null,
        json!(false),
        json!("unknown"),
        json!("200059543"),
        json!("200059545"),
        json!("200059546"),
        json!("200059547"),
    ] {
        let response = thirdlogin(code);
        for wire in [response.to_string(), wrap_web(&response)] {
            assert!(ma::web_poll_response_text(&wire).is_err());
        }
    }
    for response in [
        json!({}),
        json!({"success":true}),
        json!({"success":false,"message":"synthetic error"}),
        json!({"code":"0","success":false,"data":{"account":"synthetic-account","token":"synthetic"}}),
        json!({"code":"0","success":"true","data":{"account":"synthetic-account","token":"synthetic"}}),
        json!({"code":"0","success":null,"data":{"account":"synthetic-account","token":"synthetic"}}),
        json!({"code":"0","data":{"result":{"resultCode":"200059541"},"account":"synthetic-account","token":"synthetic"}}),
        json!({"code":"0000","data":{"result":{"resultCode":0},"account":"synthetic-account","token":"synthetic"}}),
        json!({"code":"0000","data":{"result":{},"account":"synthetic-account","token":"synthetic"}}),
        json!({"code":"0000","data":{"result":false,"account":"synthetic-account","token":"synthetic"}}),
        json!({"code":"0000","data":{"result":null,"account":"synthetic-account","token":"synthetic"}}),
        json!({"resultCode":"0","data":{"account":"synthetic-account","token":"synthetic"}}),
        json!({"code":"200059541","data":{"result":{"resultCode":"200059548"}}}),
        json!({"data":{"nested":{"code":"0","account":"synthetic-account","token":"synthetic"}}}),
    ] {
        assert!(ma::web_poll_response_text(&wrap_web(&response)).is_err());
    }
}

#[test]
fn thirdlogin_confirmation_requires_exact_valid_account_and_token_fields() {
    for (field, invalid) in [
        ("account", json!("")),
        ("account", json!("masked***")),
        ("account", json!("colon:account")),
        ("account", json!("bad\naccount")),
        ("account", json!(" leading-space")),
        ("account", json!("synthetic账户")),
        ("account", Value::Null),
        ("account", json!(42)),
        ("token", json!("")),
        ("token", json!("   ")),
        ("token", json!("bad\rtoken")),
        ("token", Value::Null),
        ("token", json!(42)),
    ] {
        let mut response = thirdlogin(json!("0000"));
        response["data"][field] = invalid;
        assert!(ma::web_poll_response_text(&wrap_web(&response)).is_err());
        response["data"] = json!(encrypt_data(&response["data"]));
        assert!(ma::web_poll_response_text(&wrap_web(&response)).is_err());
    }
    for data in [
        json!({"nested":{"account":"synthetic-account","token":"synthetic"}}),
        json!({"simplifyAccount":"synthetic-account","authToken":"synthetic"}),
        json!({"account":"synthetic-account","authToken":"synthetic"}),
        json!({"encryptAccount":STANDARD.encode("synthetic-account"),"token":"synthetic"}),
        json!([]),
        json!(false),
        Value::Null,
    ] {
        assert!(
            ma::web_poll_response_text(&wrap_web(&json!({"code":"0000","data":data}))).is_err()
        );
    }
    let result = ma::web_poll_response_text(&wrap_web(
        &json!({"code":"0000","data":encrypt_data(&thirdlogin(json!("0"))["data"])}),
    ))
    .unwrap();
    assert_eq!(result["data"]["status"], "confirmed");
}

#[test]
fn thirdlogin_decoding_limits_malformed_frames_and_strict_padding_are_safe() {
    for wire in [
        String::new(),
        "not-base64".into(),
        "null".into(),
        "[]".into(),
        "{} trailing".into(),
        "\"not-base64\"".into(),
        "\"unterminated".into(),
        "A".repeat(2 * 1024 * 1024 + 1),
        STANDARD.encode([0u8; 16]),
        STANDARD.encode([0u8; 33]),
        wrap_web_bytes(b"{malformed-json}"),
        wrap_web_bytes(b"null"),
        wrap_web_bytes(b"[]"),
        wrap_web_bytes(b"\"nested-string\""),
        wrap_web_bytes(&[0xff, 0xff]),
    ] {
        assert!(ma::web_poll_response_text(&wire).is_err());
    }
    for size in [0, 1, 15, 16, 17, 31] {
        assert!(ma::web_poll_response_text(&STANDARD.encode(vec![0u8; size])).is_err());
    }
    for data in [
        "",
        "00",
        "gggggggggggggggggggggggggggggggg",
        "éééééééééééééééé",
    ] {
        assert!(ma::web_poll_response_text(&json!({"code":"0","data":data}).to_string()).is_err());
    }
    // Corrupt the previous CBC block so the final PKCS7 byte becomes zero.
    // This is deterministic, unlike assuming a random bitflip invalidates padding.
    let mut bytes = STANDARD.decode(CBC_RESPONSE).unwrap();
    let end = bytes.len();
    let padding = 16 - json!({"code":"0000","data":ECB_DATA}).to_string().len() % 16;
    bytes[end - 17] ^= padding as u8;
    assert!(ma::web_poll_response_text(&STANDARD.encode(bytes)).is_err());
    // Independently construct invalid padded ECB blocks and encrypt without
    // applying padding, to reject zero/oversized and nonuniform PKCS7 tails.
    use aes::cipher::BlockEncrypt;
    let cipher = aes::Aes128::new_from_slice(DATA_KEY).unwrap();
    for invalid_pad in [0, 17, 255] {
        let mut block = [invalid_pad; 16];
        cipher.encrypt_block(aes::cipher::generic_array::GenericArray::from_mut_slice(
            &mut block,
        ));
        let hex: String = block.iter().map(|b| format!("{b:02x}")).collect();
        assert!(
            ma::web_poll_response_text(&json!({"code":"0000","data":hex}).to_string()).is_err()
        );
    }
    let mut mismatched = [2u8; 16];
    mismatched[14] = 3;
    cipher.encrypt_block(aes::cipher::generic_array::GenericArray::from_mut_slice(
        &mut mismatched,
    ));
    let hex: String = mismatched.iter().map(|b| format!("{b:02x}")).collect();
    assert!(ma::web_poll_response_text(&json!({"code":"0000","data":hex}).to_string()).is_err());
}

#[test]
fn thirdlogin_independent_sessions_and_ivs_remain_isolated_and_bootstrap_is_required() {
    let a = web_session();
    let mut b = session("b");
    assert!(ma::web_poll_request(&b, WEB_TIME, WEB_NONCE, &WEB_IV).is_err());
    ma::finish_web_key(&mut b, &key_response(), &vec![5; 237]).unwrap();
    let ra = ma::web_poll_request(&a, WEB_TIME, WEB_NONCE, &WEB_IV).unwrap();
    let rb = ma::web_poll_request(&b, WEB_TIME, WEB_NONCE, &WEB_IV).unwrap();
    let different_iv = ma::web_poll_request(&a, WEB_TIME, WEB_NONCE, &[42; 16]).unwrap();
    assert_ne!(ra.body, rb.body);
    assert_ne!(ra.body, different_iv.body);
    assert_eq!(
        ra.headers["mcloud-sign"],
        different_iv.headers["mcloud-sign"]
    );
    assert_ne!(ra.headers["mcloud-sign"], rb.headers["mcloud-sign"]);
    assert!(ra.headers["mcloud-skey"] != rb.headers["mcloud-skey"]);
    for (s, request) in [(&a, &ra), (&b, &rb)] {
        let inner: Value = serde_json::from_str(&decrypt_web_body(request)).unwrap();
        assert!(inner["dycpwd"].as_str() == Some(&s.sid));
        assert_eq!(inner["pintype"], 21);
        assert_eq!(inner["msisdn"], "");
        assert_eq!(inner["random"], "");
        assert!(inner.get("account").is_none());
        assert!(inner.get("token").is_none());
    }
    assert!(ma::finish_web_key(&mut b, &json!({"success":false}), &[]).is_err());
    assert!(ma::web_poll_request(&b, WEB_TIME, WEB_NONCE, &WEB_IV).is_err());
    let error = ma::web_poll_response_text(&wrap_web(&json!({"code":"private-synthetic-secret","data":{"account":"private-account","token":"private-token"}}))).unwrap_err();
    assert_eq!(error, "invalid mobile thirdlogin response");
}
