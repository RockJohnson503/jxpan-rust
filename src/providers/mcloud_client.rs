//! Read-only HTTP compatibility kernel for the pinned deployed MobileCloudParser.
//! Credentials and response bodies are deliberately absent from errors and logging.
use super::mcloud;
use crate::compat::{first_truthy_ref as choose, truthy};
use aes::Aes128;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use cbc::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};
use reqwest::{
    Client,
    header::{HeaderMap, HeaderName, HeaderValue},
};
use serde_json::{Map, Value};
use std::{fmt, sync::OnceLock, time::Duration};

const KEY: &[u8; 16] = b"PVGDwmcvfs1uV3d1";
const ENDPOINT: &str = "https://share-kd-njs.yun.139.com";
const LIST_PATH: &str = "/yun-share/richlifeApp/devapp/IOutLink/getOutLinkInfoV6";
const DOWNLOAD_PATH: &str = "/yun-share/richlifeApp/devapp/IOutLink/dlFromOutLinkV3";
// Defensive ceiling for the catch-path scan; transport may impose a tighter cap.
const MAX_RESCUE_BODY_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MobileCloudError {
    Randomness,
    Crypto,
    InvalidHeader,
    Transport,
    Http(u16),
    Json,
    ResponseShape,
    NonLoopback,
}
impl fmt::Display for MobileCloudError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Randomness => "mobile cloud entropy unavailable",
            Self::Crypto => "mobile cloud AES decode failed",
            Self::InvalidHeader => "mobile cloud header invalid",
            Self::Transport => "mobile cloud request failed or timed out",
            Self::Http(_) => "mobile cloud HTTP status not 200",
            Self::Json => "mobile cloud response JSON invalid",
            Self::ResponseShape => "mobile cloud response shape invalid",
            Self::NonLoopback => "test endpoint must be a loopback address",
        })
    }
}
impl std::error::Error for MobileCloudError {}

/// Original AESUtils2: UTF-16 .length padding THEN UTF-8 THEN CBC's own PKCS#7.
/// IV is injected only for deterministic offline vectors; production uses OS entropy.
pub fn encrypt139_with_iv(plaintext: &str, iv: [u8; 16]) -> String {
    let padding = 16 - plaintext.encode_utf16().count() % 16;
    let mut bytes = plaintext.as_bytes().to_vec();
    bytes.extend(std::iter::repeat_n(padding as u8, padding));
    let encrypted = cbc::Encryptor::<Aes128>::new(KEY.into(), (&iv).into())
        .encrypt_padded_vec_mut::<Pkcs7>(&bytes);
    let mut result = iv.to_vec();
    result.extend(encrypted);
    STANDARD.encode(result)
}

/// Removes CBC PKCS#7, then reproduces the original unvalidated manual slice.
/// Invalid input is an error here; HTTP decoding applies the original plaintext fallback.
pub fn decrypt139(encrypted: &str) -> Result<String, MobileCloudError> {
    let decoded = mcloud::decode_base64_latin1(encrypted).map_err(|_| MobileCloudError::Crypto)?;
    let bytes: Vec<u8> = decoded.chars().map(|c| c as u8).collect();
    if bytes.len() < 32 || !(bytes.len() - 16).is_multiple_of(16) {
        return Err(MobileCloudError::Crypto);
    }
    let unpadded = cbc::Decryptor::<Aes128>::new(KEY.into(), (&bytes[..16]).into())
        .decrypt_padded_vec_mut::<Pkcs7>(&bytes[16..])
        .map_err(|_| MobileCloudError::Crypto)?;
    let len = unpadded.len() as isize;
    let end = unpadded.last().map_or(0, |padding| len - *padding as isize);
    // Uint8Array.slice(0, negative) counts back from the end, not from zero.
    let end = if end < 0 {
        (len + end).max(0)
    } else {
        end.min(len)
    } as usize;
    let text = String::from_utf8_lossy(&unpadded[..end]);
    // TextDecoder's default ignoreBOM=false consumes a leading UTF-8 BOM.
    Ok(text.strip_prefix('\u{feff}').unwrap_or(&text).to_owned())
}

// String.replace replacement strings expand these four tokens even without captures.
fn replace_js(input: &str, needle: &str, replacement: &str, all: bool) -> String {
    let mut result = String::new();
    let mut cursor = 0;
    while let Some(relative) = input[cursor..].find(needle) {
        let start = cursor + relative;
        let end = start + needle.len();
        result.push_str(&input[cursor..start]);
        let mut chars = replacement.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '$' {
                let expanded = match chars.peek() {
                    Some('$') => Some("$"),
                    Some('&') => Some(&input[start..end]),
                    Some('`') => Some(&input[..start]),
                    Some('\'') => Some(&input[end..]),
                    _ => None,
                };
                if let Some(text) = expanded {
                    result.push_str(text);
                    chars.next();
                    continue;
                }
            }
            result.push(c);
        }
        cursor = end;
        if !all {
            break;
        }
    }
    result.push_str(&input[cursor..]);
    result
}

/// Literal deployed template, intentionally NOT a serde_json reserialization.
/// Values follow JS replacement semantics; quotes/backslashes are not escaped by JS.
pub fn list_request_body(account: &str, link_id: &str, pwd: &str, folder_id: &str) -> String {
    let body = r#"{"getOutLinkInfoReq":{"account":"{account}","linkID":"{key}","passwd":"{pwd}","caSrt":0,"coSrt":0,"srtDr":1,"bNum":1,"pCaID":"{pCaID}","eNum":200},"commonAccountInfo":{"account":"{account}","accountType":1}}"#;
    let body = replace_js(body, "{key}", link_id, false);
    let body = replace_js(&body, "{pwd}", pwd, false);
    let body = replace_js(&body, "{account}", account, true);
    replace_js(&body, "{pCaID}", folder_id, false)
}
pub fn download_request_body(account: &str, link_id: &str, path_or_id: &str) -> String {
    let body = r#"{"dlFromOutLinkReqV3":{"account":"{account}","linkID":"{key}","coIDLst":{"item":["{item}"]}},"commonAccountInfo":{"account":"{account}","accountType":1}}"#;
    let body = replace_js(body, "{key}", link_id, false);
    let body = replace_js(&body, "{item}", path_or_id, false);
    replace_js(&body, "{account}", account, true)
}

pub fn build_headers(authorization: &str) -> Result<HeaderMap, MobileCloudError> {
    let fields = [
        ("accept", "application/json, text/plain, */*"),
        ("accept-language", "zh-CN,zh;q=0.9,en;q=0.8"),
        ("cache-control", "no-cache"),
        ("caller", "web"),
        ("cms-device", "default"),
        ("content-type", "application/json;charset=UTF-8"),
        ("dnt", "1"),
        ("hcy-cool-flag", "1"),
        ("inner-hcy-router-https", "1"),
        ("mcloud-channel", "1000101"),
        ("mcloud-client", "10702"),
        ("mcloud-route", "001"),
        ("mcloud-version", "7.13.3"),
        ("origin", "https://yun.139.com"),
        ("pragma", "no-cache"),
        ("priority", "u=1, i"),
        ("referer", "https://yun.139.com/"),
        (
            "sec-ch-ua",
            "\"Not)A;Brand\";v=\"99\", \"Microsoft Edge\";v=\"127\", \"Chromium\";v=\"127\"",
        ),
        ("sec-ch-ua-mobile", "?0"),
        ("sec-ch-ua-platform", "Windows"),
        ("sec-fetch-dest", "empty"),
        ("sec-fetch-mode", "cors"),
        ("sec-fetch-site", "same-site"),
        (
            "user-agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/127.0.0.0 Safari/537.36 Edg/127.0.0.0",
        ),
        (
            "x-deviceinfo",
            "||3|12.27.0|edge||522b6107d153211263b13afa2b041bf5||windows 10|922X974|zh-CN|||",
        ),
        ("x-huawei-channelsrc", "10213406"),
        ("x-inner-ntwk", "2"),
        ("x-m4c-caller", "PC"),
        ("x-m4c-src", "10002"),
        ("x-svctype", "1"),
        ("x-yun-api-version", "v1"),
        ("x-yun-app-channel", "10213406"),
        ("x-yun-channel-source", "10213406"),
        (
            "x-yun-client-info",
            "||9|7.13.3|edge||522b6107d153211263b13afa2b041bf5||windows 10||zh-CN|||ZWRnZQ==||",
        ),
        ("x-yun-module-type", "100"),
        ("x-yun-svc-type", "1"),
    ];
    let mut headers = HeaderMap::new();
    for (name, value) in fields {
        headers.insert(
            HeaderName::from_static(name),
            HeaderValue::from_static(value),
        );
    }
    if let Some(auth) = mcloud::normalize_authorization_header(authorization) {
        let mut value =
            HeaderValue::from_str(&auth).map_err(|_| MobileCloudError::InvalidHeader)?;
        value.set_sensitive(true);
        headers.insert("authorization", value);
    }
    Ok(headers)
}
fn js_string(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Object(_)) => "[object Object]".into(),
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| {
                if v.is_null() {
                    String::new()
                } else {
                    js_string(Some(v))
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Some(v) => v.to_string(),
    }
}
fn folder(ca: &Value) -> Result<Value, MobileCloudError> {
    if ca.is_null() {
        return Err(MobileCloudError::ResponseShape);
    }
    let mut row = Map::new();
    // JS object serialization omits undefined coID; an explicitly null fallback stays null.
    let id = choose(ca, &["caID"]).or_else(|| ca.get("id"));
    if let Some(id) = id {
        row.insert("coID".into(), id.clone());
    }
    row.insert(
        "coName".into(),
        choose(ca, &["caName", "name"])
            .cloned()
            .unwrap_or(Value::String("未命名文件夹".into())),
    );
    row.insert("coType".into(), Value::from(1));
    row.insert("isDir".into(), Value::Bool(true));
    row.insert(
        "path".into(),
        choose(ca, &["path"])
            .cloned()
            .unwrap_or(Value::String(format!("/{}", js_string(id)))),
    );
    Ok(Value::Object(row))
}
fn successful(result: &Value) -> bool {
    result.get("success").is_some_and(truthy)
        || result.get("resultCode").and_then(Value::as_str) == Some("0")
}
// All three original regexes use JS whitespace (which includes BOM but not NEL).
fn rescue_regex(
    pattern: &'static str,
    slot: &'static OnceLock<regex::Regex>,
) -> &'static regex::Regex {
    slot.get_or_init(|| {
        let whitespace = r"[\t\n\x0B\f\r \u{00A0}\u{1680}\u{2000}-\u{200A}\u{2028}\u{2029}\u{202F}\u{205F}\u{3000}\u{FEFF}]";
        regex::Regex::new(&pattern.replace(r"\s", whitespace)).expect("fixed rescue regex")
    })
}
fn folders(cas: &[Value]) -> Result<Vec<Value>, MobileCloudError> {
    // Atomic, like JS .map(): a null entry must discard the whole mapped list.
    cas.iter().map(folder).collect()
}
/// Literal deployed catch path, bounded to the supplied body and a fixed ceiling.
/// In particular, do not repair incomplete arrays or reinterpret fileList/files.
fn rescue_list_response(text: &str) -> Vec<Value> {
    if text.len() > MAX_RESCUE_BODY_BYTES {
        return Vec::new();
    }
    static CA_LIST: OnceLock<regex::Regex> = OnceLock::new();
    static SIMPLE_FOLDER: OnceLock<regex::Regex> = OnceLock::new();
    static CO_LIST: OnceLock<regex::Regex> = OnceLock::new();
    let mut rows = Vec::new();
    if let Some(found) = rescue_regex(r#""caLst"\s*:\s*\["#, &CA_LIST).find(text) {
        let start = found.end() - 1;
        let bytes = text.as_bytes();
        let mut depth = 0usize;
        let mut in_string = false;
        let mut end = None;
        for i in start..bytes.len() {
            let ch = bytes[i];
            // Preserve the original immediate-previous-backslash rule. Do not
            // replace it with correct even/odd JSON escape counting.
            if ch == b'"' && (i == 0 || bytes[i - 1] != b'\\') {
                in_string = !in_string;
            } else if !in_string {
                if ch == b'[' {
                    depth += 1;
                }
                if ch == b']' {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(i);
                        break;
                    }
                }
            }
        }
        if let Some(end) = end {
            let parsed = serde_json::from_str::<Vec<Value>>(&text[start..=end])
                .map_err(|_| MobileCloudError::Json)
                .and_then(|cas| folders(&cas));
            match parsed {
                Ok(cas) => rows.extend(cas),
                Err(_) => {
                    let content = &text[start + 1..end];
                    if let Some(captures) = rescue_regex(
                        r#"(?s)\{"caID"\s*:\s*"([^"]+)"[^}]*?"caName"\s*:\s*"([^"]+)""#,
                        &SIMPLE_FOLDER,
                    )
                    .captures(content)
                    {
                        // Raw regex captures intentionally are not JSON-unescaped.
                        let mut ca = Map::new();
                        ca.insert("caID".into(), Value::String(captures[1].into()));
                        ca.insert("caName".into(), Value::String(captures[2].into()));
                        rows.push(folder(&Value::Object(ca)).expect("synthetic folder object"));
                    }
                }
            }
        }
    }
    if let Some(captures) = rescue_regex(r#"(?s)"coLst":\s*\[(.*?)\]"#, &CO_LIST).captures(text) {
        let array = format!("[{}]", &captures[1]);
        if let Ok(files) = serde_json::from_str::<Vec<Value>>(&array) {
            rows.extend(files);
        } else if let Some(start) = text.find("\"coLst\":[") {
            // Literal original fallback starts at +7 (the colon), not +8.
            // It also counts brackets inside strings. Keep those quirks: fixing
            // either would silently broaden which truncated responses succeed.
            let bytes = text.as_bytes();
            let mut depth = 1usize;
            let mut end = start + 7;
            while depth > 0 && end < bytes.len() {
                if bytes[end] == b'[' {
                    depth += 1;
                }
                if bytes[end] == b']' {
                    depth -= 1;
                }
                end += 1;
            }
            if depth == 0 {
                let array = format!("[{}]", &text[start + 7..end - 1]);
                if let Ok(files) = serde_json::from_str::<Vec<Value>>(&array) {
                    rows.extend(files);
                }
            }
        }
    }
    // Rescue order is folders THEN files, unlike the normal JSON path.
    rows
}
fn normalize_parsed_list(result: &Value) -> Result<Vec<Value>, MobileCloudError> {
    if result.is_null() {
        return Err(MobileCloudError::ResponseShape);
    }
    if !successful(result) {
        return Ok(Vec::new());
    }
    let data = result.get("data").unwrap_or(&Value::Null);
    let mut rows = choose(data, &["coLst", "fileList", "files", "contentList"])
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if let Some(cas) = choose(data, &["caLst"]).and_then(Value::as_array) {
        rows.extend(folders(cas)?);
    }
    Ok(rows)
}
/// Normal JSON path uses JS truthiness; parse/conversion throws use deployed rescue.
pub fn normalize_list_response(text: &str) -> Result<Vec<Value>, MobileCloudError> {
    match serde_json::from_str::<Value>(text)
        .map_err(|_| MobileCloudError::Json)
        .and_then(|result| normalize_parsed_list(&result))
    {
        Ok(rows) => Ok(rows),
        Err(_) => Ok(rescue_list_response(text)),
    }
}
/// Invalid JSON follows deployed download regex fallback, including its raw escapes.
pub fn normalize_download_response(text: &str) -> Result<Option<String>, MobileCloudError> {
    if let Ok(result) = serde_json::from_str::<Value>(text)
        && !result.is_null()
    {
        if !successful(&result) {
            return Ok(None);
        }
        let data = result.get("data").unwrap_or(&Value::Null);
        match choose(data, &["redrUrl", "downloadUrl", "url"]) {
            Some(Value::String(s)) => return Ok(Some(s.clone())),
            None => return Ok(None),
            // Deployed .substring throws for truthy nonstrings and reaches rescue.
            Some(_) => {}
        }
    }
    for name in ["redrUrl", "downloadUrl", "url"] {
        let marker = format!("\"{name}\":\"");
        if let Some(start) = text.find(&marker) {
            let tail = &text[start + marker.len()..];
            if let Some(end) = tail.find('"').filter(|end| *end > 0) {
                return Ok(Some(tail[..end].to_owned()));
            }
        }
    }
    Err(MobileCloudError::Json)
}

/// Reuses the supplied client; caller MUST configure redirect Policy::none() (or a
/// fixed-host policy) and keep normal TLS verification. No input URL is accepted.
/// A client with an unrestricted redirect policy can leave the pinned host.
#[derive(Clone)]
pub struct MobileCloudClient {
    client: Client,
    authorization: String,
    account: Option<String>,
    endpoint: String,
}
impl MobileCloudClient {
    pub fn new(client: Client, authorization: String) -> Self {
        let account = mcloud::extract_account_from_auth(&Value::String(authorization.clone()));
        Self {
            client,
            authorization,
            account,
            endpoint: ENDPOINT.into(),
        }
    }
    /// Explicit test seam, unavailable in production: numeric loopback only, no host input.
    #[cfg(test)]
    pub fn new_for_loopback(
        client: Client,
        authorization: String,
        address: std::net::SocketAddr,
    ) -> Result<Self, MobileCloudError> {
        if !address.ip().is_loopback() {
            return Err(MobileCloudError::NonLoopback);
        }
        let mut this = Self::new(client, authorization);
        this.endpoint = format!("http://{address}");
        Ok(this)
    }
    fn account(&self) -> Option<&str> {
        self.account.as_deref().filter(|s| !s.is_empty())
    }
    async fn post(&self, path: &str, body: &str) -> Result<String, MobileCloudError> {
        let mut iv = [0; 16];
        getrandom::getrandom(&mut iv).map_err(|_| MobileCloudError::Randomness)?;
        let encrypted = encrypt139_with_iv(body, iv);
        let mut response = self
            .client
            .post(format!("{}{path}", self.endpoint))
            .headers(build_headers(&self.authorization)?)
            .timeout(Duration::from_secs(30))
            .body(encrypted)
            .send()
            .await
            .map_err(|_| MobileCloudError::Transport)?;
        if response.status().as_u16() != 200 {
            return Err(MobileCloudError::Http(response.status().as_u16()));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| MobileCloudError::Transport)?
        {
            if bytes.len().saturating_add(chunk.len()) > MAX_RESCUE_BODY_BYTES {
                return Err(MobileCloudError::Json);
            }
            bytes.extend_from_slice(&chunk);
        }
        let text = String::from_utf8(bytes)
            .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned());
        if text.is_empty() {
            return Ok(text);
        }
        // AESUtils2's original catch returns input: plain JSON responses remain supported.
        Ok(decrypt139(&text).unwrap_or(text))
    }
    pub async fn list_files(
        &self,
        link_id: &str,
        pwd: &str,
        folder_id: &str,
    ) -> Result<Vec<Value>, MobileCloudError> {
        let Some(account) = self.account() else {
            return Ok(Vec::new());
        };
        let text = self
            .post(
                LIST_PATH,
                &list_request_body(account, link_id, pwd, folder_id),
            )
            .await?;
        if text.is_empty() {
            return Ok(Vec::new());
        }
        normalize_list_response(&text)
    }
    pub async fn download_url(
        &self,
        link_id: &str,
        path_or_id: &str,
    ) -> Result<Option<String>, MobileCloudError> {
        let Some(account) = self.account().filter(|_| !path_or_id.is_empty()) else {
            return Ok(None);
        };
        let text = self
            .post(
                DOWNLOAD_PATH,
                &download_request_body(account, link_id, path_or_id),
            )
            .await?;
        if text.is_empty() {
            return Ok(None);
        }
        normalize_download_response(&text)
    }
}
