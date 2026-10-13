//! Offline Mobile QR/refresh protocol kernel for the pinned deployment.
//! Time, UUIDs, nonce and RSA padding are supplied by the caller. No I/O or storage.
use super::mcloud::{decode_base64_latin1, encode_authorization, generate_mcloud_sign};
use crate::compat::{first_truthy, js_string, truthy};
use aes::cipher::{BlockEncrypt, KeyInit};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const KEY_URL: &str = "https://yun.139.com/orchestration/auth-rebuild/key/v1.0/getRsaPublicKey";
pub const LOGIN_URL: &str = "https://yun.139.com/orchestration/auth-rebuild/permission/v1.0/login";
pub const REFRESH_URL: &str = "https://aas.caiyun.feixin.10086.cn/tellin/authTokenRefresh.do";
const VERSION: &str = "7.17.2";
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36";

#[derive(Clone, PartialEq, Eq)]
pub struct Request {
    pub url: &'static str,
    pub method: &'static str,
    pub headers: BTreeMap<String, String>,
    pub body: String,
}
// Intentionally no Debug: sessions contain key material and authorization.
#[derive(Clone)]
pub struct Session {
    pub device_id: String,
    pub sid: String,
    pub aes_key: String,
    pub mcloud_skey: String,
}
impl Session {
    pub fn from_uuids(device: &str, sid: &str, key: &str) -> Self {
        fn clean(s: &str, n: usize) -> String {
            s.replace('-', "").chars().take(n).collect()
        }
        Self {
            device_id: clean(device, 32),
            sid: clean(sid, 16),
            aes_key: clean(key, 16),
            mcloud_skey: String::new(),
        }
    }
    pub fn stored_value(&self) -> Value {
        json!({"deviceId":self.device_id,"sid":self.sid,"aesKey":self.aes_key,"mcloudSkey":self.mcloud_skey})
    }
    pub fn qr_response(&self) -> Value {
        json!({"code":200,"msg":"获取二维码成功","success":true,"data":{"qr_url":format!("https://yun.139.com/w/#/qrcLogin?sID={}&dID={}&cType=9",self.sid,self.device_id),"sid":self.sid,"device_id":self.device_id,"expires_in":300,"interval":3}})
    }
}
/// Same byte modulo 62 mapping as deployed getRandomValues (not randomUUID).
pub fn nonce(bytes: &[u8; 16]) -> String {
    let chars = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    bytes
        .iter()
        .map(|b| chars[(*b as usize) % 62] as char)
        .collect()
}
fn signed_request(
    session: &Session,
    body: Value,
    timestamp: &str,
    nonce: &str,
    key: bool,
) -> Request {
    let body = body.to_string();
    let mut headers: BTreeMap<String, String> = [
        ("Content-Type", "application/json;charset=UTF-8"),
        ("Accept", "application/json, text/plain, */*"),
        ("Origin", "https://yun.139.com"),
        ("Referer", "https://yun.139.com/w/"),
        ("caller", "web"),
        ("mcloud-client", "10701"),
        ("mcloud-channel", "1000101"),
        ("mcloud-route", "001"),
        ("mcloud-version", VERSION),
        ("x-yun-channel-source", "10000034"),
        ("x-yun-svc-type", "1"),
        ("x-SvcType", "1"),
        ("User-Agent", UA),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v.into()))
    .collect();
    headers.insert(
        "mcloud-sign".into(),
        generate_mcloud_sign(Some(&body), timestamp, nonce),
    );
    headers.insert(
        "x-deviceinfo".into(),
        format!(
            "||9|{VERSION}|edge||{}||windows 10||zh-CN|||",
            session.device_id
        ),
    );
    let extras = if key {
        vec![
            ("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8"),
            ("CMS-DEVICE", "default"),
            ("DNT", "1"),
            ("INNER-HCY-ROUTER-HTTPS", "1"),
        ]
    } else {
        headers.insert("mcloud-skey".into(), session.mcloud_skey.clone());
        vec![
            ("x-huawei-channelSrc", "10000034"),
            ("x-inner-ntwk", "2"),
            ("x-m4c-caller", "PC"),
            ("x-m4c-src", "10002"),
        ]
    };
    for (k, v) in extras {
        headers.insert(k.into(), v.into());
    }
    Request {
        url: if key { KEY_URL } else { LOGIN_URL },
        method: "POST",
        headers,
        body,
    }
}
pub fn key_request(session: &Session, timestamp: &str, nonce: &str) -> Request {
    signed_request(
        session,
        json!({"clientCode":"10701","type":"1"}),
        timestamp,
        nonce,
        true,
    )
}
/// Missing publicKey leaves skey empty, as deployed. Malformed keys propagate an error.
pub fn finish_key(
    session: &mut Session,
    response: &Value,
    padding: &[u8],
) -> Result<(), &'static str> {
    if response.is_null() {
        return Err("Cannot read properties of null (reading 'success')");
    }
    if truthy(&response["success"])
        && truthy(&response["data"])
        && truthy(&response["data"]["publicKey"])
    {
        let encoded = js_string(&response["data"]["publicKey"]);
        let (n, e) = super::mcloud_auth_rsa::parse_deployed_public_key(&encoded)?;
        session.mcloud_skey =
            super::mcloud_auth_rsa::rsa_encrypt(&n, &e, session.aes_key.as_bytes(), padding)?;
    }
    Ok(())
}
pub fn qr_failure(message: &str) -> Value {
    json!({"code":500,"msg":format!("获取二维码失败: {message}"),"success":false,"data":null})
}
/// PKCS#7 AES-ECB, including a full padding block for aligned plaintext.
pub fn aes_ecb_encrypt(key: &[u8], plaintext: &[u8]) -> Result<String, &'static str> {
    let pad = 16 - plaintext.len() % 16;
    let mut blocks = plaintext.to_vec();
    blocks.resize(blocks.len() + pad, pad as u8);
    macro_rules! enc {
        ($cipher:ty) => {{
            let cipher = <$cipher>::new_from_slice(key).map_err(|_| "invalid AES key")?;
            for block in blocks.chunks_exact_mut(16) {
                cipher.encrypt_block(aes::cipher::generic_array::GenericArray::from_mut_slice(
                    block,
                ));
            }
        }};
    }
    match key.len() {
        16 => enc!(aes::Aes128),
        24 => enc!(aes::Aes192),
        32 => enc!(aes::Aes256),
        _ => return Err("invalid AES key"),
    }
    Ok(STANDARD.encode(blocks))
}
pub fn poll_request(
    session: &Session,
    timestamp: &str,
    nonce: &str,
) -> Result<Request, &'static str> {
    let inner = json!({"dycPwd":session.sid,"loginStyle":"QRCode","clientEnv":"3","setCookie":0})
        .to_string();
    let encrypted = aes_ecb_encrypt(session.aes_key.as_bytes(), inner.as_bytes())?;
    Ok(signed_request(
        session,
        json!({"encryptMsg":encrypted,"clientId":"10701","returnToken":true}),
        timestamp,
        nonce,
        false,
    ))
}
fn status(code: i32, msg: &str, success: bool, state: &str) -> Value {
    json!({"code":code,"msg":msg,"success":success,"data":{"status":state}})
}
pub fn poll_missing_storage() -> Value {
    status(500, "存储未配置", false, "waiting")
}
pub fn poll_missing_session() -> Value {
    status(400, "会话已过期，请重新获取二维码", false, "expired")
}
pub fn poll_failure(message: &str) -> Value {
    status(500, &format!("轮询失败: {message}"), false, "waiting")
}
/// Strict string result codes. Numeric 0 is NOT a confirmed login.
pub fn poll_response(login: &Value) -> Value {
    if login.is_null() {
        return poll_failure("Cannot read properties of null (reading 'data')");
    }
    let data = &login["data"];
    let code = first_truthy(&data["result"], &["resultCode"], json!(""));
    if truthy(&login["success"]) && code == "0" {
        let token = first_truthy(data, &["authToken", "token"], Value::Null);
        if truthy(&token) {
            let account = first_truthy(data, &["encryptAccount"], json!(""));
            let phone = decode_base64_latin1(&js_string(&account)).unwrap_or_else(|_| {
                js_string(&first_truthy(data, &["simplifyAccount"], json!("")))
            });
            match encode_authorization(&format!("pc:{phone}:{}", js_string(&token))) {
                Ok(auth) => {
                    return json!({"code":200,"msg":"登录成功","success":true,"data":{"status":"confirmed","authorization":auth,"phone":phone}});
                }
                Err(_) => return poll_failure("Invalid character"),
            }
        }
    }
    match code.as_str() {
        Some("200059548") => status(200, "已扫码", true, "scanned"),
        Some("200059542" | "200059549") => status(200, "已过期", true, "expired"),
        _ => status(200, "等待扫码", true, "waiting"),
    }
}
fn js_space(c: char) -> bool {
    matches!(c,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}')
}
fn number(v: &Value) -> f64 {
    match v {
        Value::Null => 0.,
        Value::Bool(b) => {
            if *b {
                1.
            } else {
                0.
            }
        }
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        _ => {
            let s = js_string(v);
            let s = s.trim_matches(js_space);
            if s.is_empty() {
                return 0.;
            }
            for (p, r) in [
                ("0x", 16),
                ("0X", 16),
                ("0b", 2),
                ("0B", 2),
                ("0o", 8),
                ("0O", 8),
            ] {
                if let Some(s) = s.strip_prefix(p) {
                    return u64::from_str_radix(s, r)
                        .map(|n| n as f64)
                        .unwrap_or(f64::NAN);
                }
            }
            s.parse().unwrap_or(f64::NAN)
        }
    }
}
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
pub struct RefreshPlan {
    pub info: Value,
    pub authorization: Option<String>,
    pub request: Option<Request>,
    pub changed: bool,
    prefix: String,
    account: String,
    now: i64,
}
pub struct RefreshResult {
    pub authorization: Option<String>,
    pub write_info: Option<Value>,
    pub refreshed: bool,
}
/// Input is the already-decoded storage value. Missing storage is represented by None.
pub fn prepare_refresh(stored: Option<&Value>, now: i64) -> RefreshPlan {
    let mut plan = RefreshPlan {
        info: Value::Null,
        authorization: None,
        request: None,
        changed: false,
        prefix: String::new(),
        account: String::new(),
        now,
    };
    let Some(stored) = stored else { return plan };
    plan.info = if stored.is_object() || stored.is_array() {
        stored.clone()
    } else if stored.is_string() {
        json!({"authorization":stored})
    } else {
        return plan;
    };
    let auth = first_truthy(&plan.info, &["authorization", "auth"], json!(""));
    let Some(auth) = auth.as_str().filter(|s| !s.is_empty()) else {
        return plan;
    };
    plan.authorization = Some(auth.into());
    // No outer trim: /^basic\s+/i, unlike the account extractor.
    let encoded = if auth
        .get(..5)
        .is_some_and(|s| s.eq_ignore_ascii_case("basic"))
        && auth[5..].chars().next().is_some_and(js_space)
    {
        auth[5..].trim_start_matches(js_space)
    } else {
        auth
    };
    let Ok(decoded) = decode_base64_latin1(encoded) else {
        return plan;
    };
    let parts: Vec<_> = decoded.split(':').collect();
    plan.prefix = parts[0].to_owned();
    if plan.prefix.is_empty() {
        plan.prefix = "pc".into();
    }
    plan.account = parts
        .get(1)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| js_string(&first_truthy(&plan.info, &["phone"], json!(""))));
    let token = parts.get(2..).unwrap_or(&[]).join(":");
    if plan.account.is_empty() || token.is_empty() {
        return plan;
    }
    plan.changed = !truthy(&plan.info["jxpan_auto_refresh_managed"]);
    let expiration = token
        .split('|')
        .nth(3)
        .map(|s| number(&json!(s)))
        .unwrap_or(f64::NAN);
    if expiration.is_finite()
        && expiration > 0.
        && expiration <= now as f64 + 15. * 86400. * 1000.
        && now as f64
            - number(&first_truthy(
                &plan.info,
                &["mcloud_auto_refresh_failed_at"],
                json!(0),
            ))
            >= 3600000.
    {
        plan.request = Some(Request {
            url: REFRESH_URL,
            method: "POST",
            headers: [
                (
                    "Content-Type".into(),
                    "application/xml;charset=UTF-8".into(),
                ),
                ("Accept".into(), "application/xml,text/xml,*/*".into()),
            ]
            .into(),
            body: format!(
                "<root><token>{}</token><account>{}</account><clienttype>656</clienttype></root>",
                xml_escape(&token),
                xml_escape(&plan.account)
            ),
        });
    }
    // Marking managed is independent of whether the refresh window is reached.
    if plan.info.is_object() {
        plan.info["jxpan_auto_refresh_managed"] = json!(true);
    }
    plan
}
/// None response models a transport error. Returns a write intent, never writes storage.
pub fn finish_refresh(mut plan: RefreshPlan, response: Option<(bool, &str)>) -> RefreshResult {
    let old = plan.authorization.clone();
    if plan.request.is_some() {
        let token = response.and_then(|(ok, text)| {
            if !ok {
                return None;
            }
            // JS \s includes BOM, but excludes NEL (Rust regex \s differs).
            let ws = r"[\x{0009}-\x{000d}\x{0020}\x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]*";
            let r = regex::Regex::new(&format!(r"(?i)<return>{ws}([^<]+?){ws}</return>")).ok()?;
            let t = regex::Regex::new(&format!(r"(?is)<token>{ws}(.*?){ws}</token>")).ok()?;
            let ret = r.captures(text)?;
            let ret = ret[1].trim_matches(js_space);
            if ret != "0" {
                return None;
            }
            let token = t.captures(text)?.get(1)?.as_str();
            if token.is_empty() {
                None
            } else {
                Some(token.trim_matches(js_space).to_owned())
            }
        });
        if let Some(auth) = token.and_then(|token| {
            encode_authorization(&format!("{}:{}:{token}", plan.prefix, plan.account)).ok()
        }) {
            plan.authorization = Some(auth.clone());
            plan.info["authorization"] = json!(auth);
            if !truthy(&plan.info["phone"]) {
                plan.info["phone"] = json!(plan.account);
            }
            plan.info["mcloud_auto_refreshed_at"] = json!(plan.now);
            if let Some(obj) = plan.info.as_object_mut() {
                obj.remove("mcloud_auto_refresh_failed_at");
            }
        } else {
            plan.info["mcloud_auto_refresh_failed_at"] = json!(plan.now);
        }
        plan.changed = true;
    }
    let refreshed = old != plan.authorization;
    let write_info = if plan.changed {
        plan.info["timestamp"] = json!(plan.now);
        Some(plan.info)
    } else {
        None
    };
    RefreshResult {
        authorization: plan.authorization,
        write_info,
        refreshed,
    }
}
