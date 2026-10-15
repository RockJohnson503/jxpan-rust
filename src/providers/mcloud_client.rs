//! Strict, complete read-only Mobile Cloud HTTP client.
//! Public template/normalization helpers below are frozen compatibility vectors,
//! not the production listing or download path. Errors never include credentials/bodies.
use super::mcloud;
use crate::compat::{first_truthy_ref as choose, truthy};
use crate::settings::{UpstreamLimit, UpstreamPermit};
use aes::Aes128;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use cbc::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};
use reqwest::{
    Client,
    header::{HeaderMap, HeaderName, HeaderValue},
};
use serde_json::{Map, Value, json};
use std::{
    collections::HashSet,
    fmt,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

const KEY: &[u8; 16] = b"PVGDwmcvfs1uV3d1";
const ENDPOINT: &str = "https://share-kd-njs.yun.139.com";
const LIST_PATH: &str = "/yun-share/richlifeApp/devapp/IOutLink/getOutLinkInfoV6";
const DOWNLOAD_PATH: &str = "/yun-share/richlifeApp/devapp/IOutLink/dlFromOutLinkV3";
// Defensive ceiling for the catch-path scan; transport may impose a tighter cap.
const MAX_RESCUE_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAX_RESPONSE_BODY_BYTES: usize = 2 * 1024 * 1024;
const MAX_LIST_BYTES: usize = 4 * 1024 * 1024;
const PAGE_SIZE: usize = 200;
const MAX_LIST_PAGES: usize = 100;
const MAX_LIST_ENTRIES: usize = 20_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MobileCloudError {
    Randomness,
    Crypto,
    InvalidHeader,
    Transport,
    Http(u16),
    Json,
    ResponseShape,
    Upstream,
    Pagination,
    Limit,
    DownloadUrl,
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
            Self::Upstream => "mobile cloud upstream rejected the request",
            Self::Pagination => "mobile cloud pagination inconsistent; no partial result returned",
            Self::Limit => "mobile cloud complete listing exceeds resource limit",
            Self::DownloadUrl => "mobile cloud download URL invalid",
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

/// Reproduces the original manual slice for frozen compatibility vectors.
/// Production responses also support single-layer CBC JSON via decrypt139_response.
pub fn decrypt139(encrypted: &str) -> Result<String, MobileCloudError> {
    Ok(legacy_plaintext(&decrypt139_frame(encrypted)?))
}
fn decrypt139_frame(encrypted: &str) -> Result<Vec<u8>, MobileCloudError> {
    let decoded = mcloud::decode_base64_latin1(encrypted).map_err(|_| MobileCloudError::Crypto)?;
    let bytes: Vec<u8> = decoded.chars().map(|c| c as u8).collect();
    if bytes.len() < 32 || !(bytes.len() - 16).is_multiple_of(16) {
        return Err(MobileCloudError::Crypto);
    }
    cbc::Decryptor::<Aes128>::new(KEY.into(), (&bytes[..16]).into())
        .decrypt_padded_vec_mut::<Pkcs7>(&bytes[16..])
        .map_err(|_| MobileCloudError::Crypto)
}
fn legacy_plaintext(unpadded: &[u8]) -> String {
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
    text.strip_prefix('\u{feff}').unwrap_or(&text).to_owned()
}
/// Production supports both ordinary single-layer CBC/PKCS#7 JSON and the
/// deployed double-padding format. Decoding does not imply JSON/listing success.
fn decrypt139_response(encrypted: &str) -> Result<String, MobileCloudError> {
    let unpadded = decrypt139_frame(encrypted)?;
    let text = std::str::from_utf8(&unpadded).map_err(|_| MobileCloudError::Crypto)?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    if serde_json::from_str::<Value>(text).is_ok() {
        Ok(text.to_owned())
    } else {
        // Only remove a real, uniform legacy padding layer. The frozen manual
        // slice can otherwise turn malformed trailing bytes into valid JSON.
        let padding = unpadded
            .last()
            .copied()
            .filter(|n| (1..=16).contains(n))
            .ok_or(MobileCloudError::Crypto)? as usize;
        let end = unpadded
            .len()
            .checked_sub(padding)
            .ok_or(MobileCloudError::Crypto)?;
        if !unpadded[end..].iter().all(|byte| *byte as usize == padding) {
            return Err(MobileCloudError::Crypto);
        }
        let text = std::str::from_utf8(&unpadded[..end]).map_err(|_| MobileCloudError::Crypto)?;
        Ok(text.strip_prefix('\u{feff}').unwrap_or(text).to_owned())
    }
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

/// FROZEN compatibility template; NOT used by production list_files.
/// Values follow JS replacement semantics; quotes/backslashes are intentionally unescaped.
pub fn list_request_body(account: &str, link_id: &str, pwd: &str, folder_id: &str) -> String {
    let body = r#"{"getOutLinkInfoReq":{"account":"{account}","linkID":"{key}","passwd":"{pwd}","caSrt":0,"coSrt":0,"srtDr":1,"bNum":1,"pCaID":"{pCaID}","eNum":200},"commonAccountInfo":{"account":"{account}","accountType":1}}"#;
    let body = replace_js(body, "{key}", link_id, false);
    let body = replace_js(&body, "{pwd}", pwd, false);
    let body = replace_js(&body, "{account}", account, true);
    replace_js(&body, "{pCaID}", folder_id, false)
}
/// FROZEN compatibility template; NOT used by production download_url.
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
/// FROZEN compatibility only: JS truthiness and deployed partial-JSON rescue.
/// Production list_files must never interpret these rescued rows as a complete directory.
pub fn normalize_list_response(text: &str) -> Result<Vec<Value>, MobileCloudError> {
    match serde_json::from_str::<Value>(text)
        .map_err(|_| MobileCloudError::Json)
        .and_then(|result| normalize_parsed_list(&result))
    {
        Ok(rows) => Ok(rows),
        Err(_) => Ok(rescue_list_response(text)),
    }
}
/// FROZEN compatibility only: deployed download regex fallback, including raw escapes.
/// Production download_url parses complete JSON and validates a standard HTTP(S) URL.
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

// Production request builders use serde, never JS string replacements. Each range
// is inclusive; caLength/coLength describe the current page, while nodNum can count
// recursive descendants and is deliberately not interpreted as a listing total.
fn list_page_body(
    account: &str,
    link_id: &str,
    pwd: &str,
    folder_id: &str,
    page: usize,
    cursor: Option<&Value>,
) -> String {
    let mut body = json!({
        "getOutLinkInfoReq": {"account":account,"linkID":link_id,"passwd":pwd,
            "caSrt":0,"coSrt":0,"srtDr":1,"bNum":page * PAGE_SIZE + 1,
            "pCaID":folder_id,"eNum":(page + 1) * PAGE_SIZE},
        "commonAccountInfo":{"account":account,"accountType":1}
    });
    if let Some(cursor) = cursor {
        body["getOutLinkInfoReq"]["nextPageCursor"] = cursor.clone();
    }
    body.to_string()
}
fn download_body(account: &str, link_id: &str, path_or_id: &str) -> String {
    json!({"dlFromOutLinkReqV3":{"account":account,"linkID":link_id,
        "coIDLst":{"item":[path_or_id]}},
        "commonAccountInfo":{"account":account,"accountType":1}})
    .to_string()
}
fn checked_data(result: &Value) -> Result<&Map<String, Value>, MobileCloudError> {
    if !result.is_object() {
        return Err(MobileCloudError::ResponseShape);
    }
    let code = result.get("resultCode");
    let zero = |value: &Value| value.as_str() == Some("0") || value.as_u64() == Some(0);
    // Explicit rejection wins, even when another marker claims success.
    if result.get("success") == Some(&Value::Bool(false))
        || code.is_some_and(|value| !zero(value))
        || (result.get("success") != Some(&Value::Bool(true)) && !code.is_some_and(zero))
    {
        return Err(MobileCloudError::Upstream);
    }
    result
        .get("data")
        .and_then(Value::as_object)
        .ok_or(MobileCloudError::ResponseShape)
}
fn checked_list<'a>(
    data: &'a Map<String, Value>,
    name: &str,
) -> Result<&'a [Value], MobileCloudError> {
    match data.get(name) {
        Some(Value::Array(rows)) if rows.len() <= PAGE_SIZE => Ok(rows),
        Some(Value::Null) => Ok(&[]),
        _ => Err(MobileCloudError::ResponseShape),
    }
}
fn checked_page_count(
    data: &Map<String, Value>,
    name: &str,
    actual_count: usize,
) -> Result<(), MobileCloudError> {
    let count = match data.get(name) {
        None | Some(Value::Null) => return Ok(()),
        Some(Value::Number(number)) => number.as_u64(),
        Some(Value::String(text))
            if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) =>
        {
            text.parse().ok()
        }
        _ => None,
    }
    .ok_or(MobileCloudError::ResponseShape)?;
    let count = usize::try_from(count).map_err(|_| MobileCloudError::Limit)?;
    if count > MAX_LIST_ENTRIES {
        return Err(MobileCloudError::Limit);
    }
    if count != actual_count {
        return Err(MobileCloudError::Pagination);
    }
    Ok(())
}
// Canonical, checked metadata (including lossless decimal sizes), with only the
// optional paths needed by explicit downloads retained. Legacy raw rows stay in
// normalize_list_response, not in this production adapter.
fn checked_entry(entry: &Value, catalog: bool) -> Result<Value, MobileCloudError> {
    let object = entry.as_object().ok_or(MobileCloudError::ResponseShape)?;
    let folder;
    let source = if catalog {
        folder = json!({"coID":object.get("caID").or_else(|| object.get("id")),
            "coName":object.get("caName").or_else(|| object.get("name")),"isDir":true});
        &folder
    } else {
        // coLst is the supplier's file list; coType is a content subtype, not
        // a directory marker. Explicit list provenance wins over that subtype.
        folder = {
            let mut file = entry.clone();
            for key in ["isFolder", "isDir", "isDirectory"] {
                file[key] = json!(false);
            }
            file
        };
        &folder
    };
    let mut row =
        crate::tree::normalize_entry(source).map_err(|_| MobileCloudError::ResponseShape)?;
    row["isDir"] = row["isFolder"].clone();
    for key in ["path", "coPath"] {
        match object.get(key) {
            Some(Value::String(path)) => row[key] = json!(path),
            None | Some(Value::Null) => {}
            _ => return Err(MobileCloudError::ResponseShape),
        }
    }
    Ok(row)
}
struct ListPage {
    rows: Vec<Value>,
    ca_count: usize,
    co_count: usize,
    cursor: Option<Value>,
}
fn checked_page(text: &str) -> Result<ListPage, MobileCloudError> {
    let result: Value = serde_json::from_str(text).map_err(|_| MobileCloudError::Json)?;
    let data = checked_data(&result)?;
    // Both keys must be explicit; null is a supplier-supported empty list.
    let cas = checked_list(data, "caLst")?;
    let cos = checked_list(data, "coLst")?;
    // Optional supplier lengths are per-list counts for this page, not totals.
    checked_page_count(data, "caLength", cas.len())?;
    checked_page_count(data, "coLength", cos.len())?;
    let rows = cos
        .iter()
        .map(|row| checked_entry(row, false))
        .chain(cas.iter().map(|row| checked_entry(row, true)))
        .collect::<Result<Vec<_>, _>>()?;
    let cursor = data
        .get("nextPageCursor")
        .filter(|v| !v.is_null() && v.as_str() != Some(""))
        .cloned(); // Opaque JSON: echo without interpolation or coercion.
    Ok(ListPage {
        rows,
        ca_count: cas.len(),
        co_count: cos.len(),
        cursor,
    })
}
fn checked_download(text: &str) -> Result<Option<String>, MobileCloudError> {
    let result: Value = serde_json::from_str(text).map_err(|_| MobileCloudError::Json)?;
    let data = checked_data(&result)?;
    static AMP: OnceLock<regex::Regex> = OnceLock::new();
    let amp = AMP.get_or_init(|| {
        regex::Regex::new(r"&(?:amp|AMP|#0*38|#[xX]0*26);").expect("fixed amp regex")
    });
    for name in ["redrUrl", "downloadUrl", "url"] {
        let raw = match data.get(name) {
            None | Some(Value::Null) => continue,
            Some(Value::String(raw)) if raw.is_empty() => continue,
            Some(Value::String(raw)) => raw,
            _ => return Err(MobileCloudError::ResponseShape),
        };
        // Decode only one HTML ampersand layer, AFTER JSON unescaping. Never
        // alter percent escapes, expand JS replacement tokens, or double-decode.
        let raw = amp.replace_all(raw, "&");
        if raw
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '\\')
        {
            return Err(MobileCloudError::DownloadUrl);
        }
        let prefix = if raw
            .get(..7)
            .is_some_and(|s| s.eq_ignore_ascii_case("http://"))
        {
            7
        } else if raw
            .get(..8)
            .is_some_and(|s| s.eq_ignore_ascii_case("https://"))
        {
            8
        } else {
            return Err(MobileCloudError::DownloadUrl);
        };
        // WHATWG URL parsers repair https:///host and https://@host; do not
        // silently reinterpret malformed or credential-bearing authorities.
        let authority = raw[prefix..].split(['/', '?', '#']).next().unwrap_or("");
        if authority.is_empty() || authority.contains('@') {
            return Err(MobileCloudError::DownloadUrl);
        }
        let url = url::Url::parse(&raw).map_err(|_| MobileCloudError::DownloadUrl)?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(MobileCloudError::DownloadUrl);
        }
        // URL parsing is validation only: reserialization would normalize signed
        // path segments, scheme/host case and default ports.
        return Ok(Some(raw.into_owned()));
    }
    Ok(None)
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
    limiter: UpstreamLimit,
    auth_rejection: Option<Arc<AtomicBool>>,
}
impl MobileCloudClient {
    pub fn new(client: Client, authorization: String) -> Self {
        let account = mcloud::extract_account_from_auth(&Value::String(authorization.clone()));
        Self {
            client,
            authorization,
            account,
            endpoint: ENDPOINT.into(),
            limiter: UpstreamLimit::default(),
            auth_rejection: None,
        }
    }
    /// Share the process-wide budget with all providers/requests; clones retain it.
    pub fn with_limit(mut self, limiter: UpstreamLimit) -> Self {
        self.limiter = limiter;
        self
    }
    /// Attach a caller-owned, per-attempt signal; clones share only this attempt.
    pub(crate) fn with_auth_rejection(mut self, rejected: Arc<AtomicBool>) -> Self {
        self.auth_rejection = Some(rejected);
        self
    }

    pub fn concurrency(&self) -> usize {
        self.limiter.concurrency()
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
    fn account(&self) -> &str {
        self.account
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("default")
    }
    async fn post(&self, path: &str, body: &str) -> Result<String, MobileCloudError> {
        let mut iv = [0; 16];
        getrandom::getrandom(&mut iv).map_err(|_| MobileCloudError::Randomness)?;
        let encrypted = encrypt139_with_iv(body, iv);
        // The permit spans send AND every response-body chunk. Dropping this
        // future (including cancellation) releases the shared budget by RAII.
        let _permit: UpstreamPermit = self.limiter.acquire().await;
        let mut response = self
            .client
            .post(format!("{}{path}", self.endpoint))
            .headers(build_headers(&self.authorization)?)
            .timeout(Duration::from_secs(30))
            .body(encrypted)
            .send()
            .await
            .map_err(|_| MobileCloudError::Transport)?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED
            && let Some(rejected) = &self.auth_rejection
        {
            rejected.store(true, Ordering::Relaxed);
        }
        if response.status().as_u16() != 200 {
            return Err(MobileCloudError::Http(response.status().as_u16()));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| MobileCloudError::Transport)?
        {
            if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BODY_BYTES {
                return Err(MobileCloudError::Json);
            }
            bytes.extend_from_slice(&chunk);
        }
        let text = String::from_utf8(bytes).map_err(|_| MobileCloudError::Json)?;
        if text.is_empty() {
            return Ok(text);
        }
        // AESUtils2's original catch returns input: plain JSON responses remain supported.
        Ok(decrypt139_response(&text).unwrap_or(text))
    }
    pub async fn list_files(
        &self,
        link_id: &str,
        pwd: &str,
        folder_id: &str,
    ) -> Result<Vec<Value>, MobileCloudError> {
        let mut rows = Vec::new();
        let mut ids = HashSet::new();
        let mut cursors = HashSet::new();
        let mut cursor = None;
        let (mut ca_seen, mut co_seen) = (0, 0);
        let mut retained_bytes = 0usize;
        for page_number in 0..MAX_LIST_PAGES {
            let text = self
                .post(
                    LIST_PATH,
                    &list_page_body(
                        self.account(),
                        link_id,
                        pwd,
                        folder_id,
                        page_number,
                        cursor.as_ref(),
                    ),
                )
                .await?;
            let page = checked_page(&text)?;
            ca_seen += page.ca_count;
            co_seen += page.co_count;
            if ca_seen + co_seen > MAX_LIST_ENTRIES {
                return Err(MobileCloudError::Limit);
            }
            for row in &page.rows {
                retained_bytes = retained_bytes.saturating_add(row.to_string().len() + 256);
                if retained_bytes > MAX_LIST_BYTES {
                    return Err(MobileCloudError::Limit);
                }
                let id = row["fileId"].as_str().expect("checked entry ID");
                // Includes repeated pages, overlapping pages and folder/file collisions.
                if !ids.insert(id.to_owned()) {
                    return Err(MobileCloudError::Pagination);
                }
            }
            // A full individual list requires another page, even with a matching
            // page count. Only a short page without an active cursor ends listing.
            let more =
                page.cursor.is_some() || page.ca_count == PAGE_SIZE || page.co_count == PAGE_SIZE;
            if more {
                if page.rows.is_empty() {
                    return Err(MobileCloudError::Pagination);
                }
                if let Some(cursor) = &page.cursor
                    && !cursors.insert(cursor.to_string())
                {
                    return Err(MobileCloudError::Pagination);
                }
            }
            rows.extend(page.rows);
            if !more {
                return Ok(rows);
            }
            cursor = page.cursor;
        }
        Err(MobileCloudError::Limit)
    }
    pub async fn download_url(
        &self,
        link_id: &str,
        path_or_id: &str,
    ) -> Result<Option<String>, MobileCloudError> {
        if path_or_id.is_empty() {
            return Ok(None);
        }
        let text = self
            .post(
                DOWNLOAD_PATH,
                &download_body(self.account(), link_id, path_or_id),
            )
            .await?;
        checked_download(&text)
    }

    // Test-only transport adapters for the unchanged oracle. These intentionally
    // preserve old short-circuits/templates/rescue; production cannot call them.
    #[cfg(test)]
    pub async fn list_files_legacy(
        &self,
        link_id: &str,
        pwd: &str,
        folder_id: &str,
    ) -> Result<Vec<Value>, MobileCloudError> {
        let Some(account) = self.account.as_deref().filter(|s| !s.is_empty()) else {
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
    #[cfg(test)]
    pub async fn download_url_legacy(
        &self,
        link_id: &str,
        path_or_id: &str,
    ) -> Result<Option<String>, MobileCloudError> {
        let Some(account) = self
            .account
            .as_deref()
            .filter(|s| !s.is_empty() && !path_or_id.is_empty())
        else {
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

#[cfg(test)]
mod tests {
    use super::*;

    async fn loopback_response(
        status: u16,
        body: String,
    ) -> (MobileCloudClient, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = axum::Router::new().fallback(move || {
            let body = body.clone();
            async move { (axum::http::StatusCode::from_u16(status).unwrap(), body) }
        });
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        (
            MobileCloudClient::new_for_loopback(client, String::new(), address).unwrap(),
            server,
        )
    }

    #[tokio::test]
    async fn http_401_marks_list_and_download_but_other_statuses_do_not() {
        for status in [401, 403, 429, 500] {
            let (client, server) = loopback_response(status, "not JSON".into()).await;
            for download in [false, true] {
                let rejected = Arc::new(AtomicBool::new(false));
                let client = client.clone().with_auth_rejection(rejected.clone());
                let error = if download {
                    client.download_url("share", "file").await.unwrap_err()
                } else {
                    client.list_files("share", "", "folder").await.unwrap_err()
                };
                assert_eq!(error, MobileCloudError::Http(status));
                assert_eq!(rejected.load(Ordering::Relaxed), status == 401);
            }
            server.abort();
        }
    }

    #[tokio::test]
    async fn business_codes_messages_and_decode_failures_do_not_mark_rejection() {
        for body in [
            r#"{"resultCode":"401","message":"authentication required"}"#.to_owned(),
            r#"{"resultCode":401}"#.to_owned(),
            r#"{"resultCode":"UNKNOWN","message":"InvalidSessionKey"}"#.to_owned(),
            r#"{"resultCode":"InvalidAccessToken"}"#.to_owned(),
            r#"{"resultCode":"ShareAccessCodeError"}"#.to_owned(),
            r#"{"resultCode":"ShareExpired"}"#.to_owned(),
            r#"{"resultCode":"401""#.to_owned(),
            "not valid encrypted data".to_owned(),
            String::new(),
            encrypt139_with_iv(r#"{"resultCode":"401"}"#, [7; 16]),
            encrypt139_with_iv("not JSON", [8; 16]),
        ] {
            let (client, server) = loopback_response(200, body.clone()).await;
            let rejected = Arc::new(AtomicBool::new(false));
            let client = client.with_auth_rejection(rejected.clone());
            assert!(client.list_files("share", "", "folder").await.is_err());
            assert!(client.download_url("share", "file").await.is_err());
            assert!(!rejected.load(Ordering::Relaxed), "{body}");
            server.abort();
        }
    }

    #[tokio::test]
    async fn transport_failure_does_not_mark_rejection() {
        let (client, server) = loopback_response(200, String::new()).await;
        server.abort();
        let _ = server.await;
        let rejected = Arc::new(AtomicBool::new(false));
        let client = client.with_auth_rejection(rejected.clone());
        assert_eq!(
            client.list_files("share", "", "folder").await.unwrap_err(),
            MobileCloudError::Transport
        );
        assert!(!rejected.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn auth_rejection_clones_share_only_the_attached_attempt() {
        let (client, server) = loopback_response(401, String::new()).await;
        assert!(client.auth_rejection.is_none());
        let first = Arc::new(AtomicBool::new(false));
        let second = Arc::new(AtomicBool::new(false));
        let attempt = client.with_auth_rejection(first.clone());
        let cloned = attempt.clone();
        let next_attempt = attempt.clone().with_auth_rejection(second.clone());
        assert!(Arc::ptr_eq(
            attempt.auth_rejection.as_ref().unwrap(),
            cloned.auth_rejection.as_ref().unwrap()
        ));
        assert_eq!(
            cloned.list_files("share", "", "folder").await.unwrap_err(),
            MobileCloudError::Http(401)
        );
        assert!(first.load(Ordering::Relaxed));
        assert!(!second.load(Ordering::Relaxed));
        assert_eq!(
            next_attempt
                .download_url("share", "file")
                .await
                .unwrap_err(),
            MobileCloudError::Http(401)
        );
        assert!(second.load(Ordering::Relaxed));
        server.abort();
    }
}
