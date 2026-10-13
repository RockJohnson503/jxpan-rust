//! Read-only network adapter for the deployed Cloud189Parser. No login, refresh,
//! transfer, deletion, tree cache, or download-body requests are performed here.
use crate::compat::truthy;
use crate::providers::cloud189;
use reqwest::{Client, Method, RequestBuilder, Response};
use serde_json::{Value, json};
use std::{
    fmt,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use url::Url;

const APP: &str = "https://api.cloud.189.cn";
const WEB: &str = "https://cloud.189.cn/api";
const ACCEPT: &str = "application/json;charset=UTF-8";
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const UA_V1: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/147.0.0.0 Safari/537.36 Edg/147.0.0.0";
const UA_OPEN: &str = "Mozilla/5.0 (Linux; Android 6.0; Nexus 5 Build/MRA58N) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/134.0.0.0 Mobile Safari/537.36 Edg/134.0.0.0";
const LIST: &str = "/open/share/listShareDir.action";
const DOWNLOAD: &str = "/getFileDownloadUrl.action";
const MAX_JSON: usize = 2 * 1024 * 1024;

/// Intentionally contains neither request URLs nor response bodies/credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cloud189Error {
    pub operation: &'static str,
    pub kind: &'static str,
}
impl fmt::Display for Cloud189Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Cloud189 {}: {}", self.operation, self.kind)
    }
}
impl std::error::Error for Cloud189Error {}
type Result<T> = std::result::Result<T, Cloud189Error>;
fn error(operation: &'static str, kind: &'static str) -> Cloud189Error {
    Cloud189Error { operation, kind }
}

/// Credentials are instance-local. Deliberately not Debug (even synthetic secrets
/// should not become log output). Reuse the caller's pool; configure that client
/// with redirect::Policy::none() and without a shared cookie jar/default secrets.
#[derive(Clone)]
pub struct Cloud189Client {
    client: Client,
    session_key: String,
    session_secret: String,
    open_access_token: String,
    cookie_login_user: String,
    share_cookies: String,
    #[cfg(test)]
    test_base: Option<Url>,
}

impl Cloud189Client {
    pub fn new(client: Client, credentials: Value) -> Self {
        let string = |key: &str| {
            credentials
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        };
        Self {
            client,
            session_key: string("session_key"),
            session_secret: string("session_secret"),
            open_access_token: string("open_access_token"),
            cookie_login_user: string("cookie_login_user"),
            share_cookies: string("share_session_cookies"),
            #[cfg(test)]
            test_base: None,
        }
    }

    // Explicitly test-only and crate-private: it cannot redirect production traffic.
    #[cfg(test)]
    pub fn for_loopback_test(client: Client, credentials: Value, base: &str) -> Self {
        let url = Url::parse(base).expect("test URL");
        assert_eq!(url.scheme(), "http");
        assert!(
            url.host_str()
                .and_then(|host| host.parse::<std::net::IpAddr>().ok())
                .is_some_and(|ip| ip.is_loopback())
        );
        assert!(url.username().is_empty() && url.password().is_none());
        assert_eq!(url.path(), "/");
        assert!(url.query().is_none() && url.fragment().is_none());
        let mut value = Self::new(client, credentials);
        value.test_base = Some(url);
        value
    }

    fn now(&self) -> SystemTime {
        #[cfg(test)]
        if self.test_base.is_some() {
            return UNIX_EPOCH + Duration::from_millis(1_700_000_000_123);
        }
        SystemTime::now()
    }
    fn timestamp(&self) -> String {
        self.now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .to_string()
    }
    fn uuid(&self) -> String {
        #[cfg(test)]
        if self.test_base.is_some() {
            return "44444444-4444-4444-8444-444444444444".to_owned();
        }
        uuid::Uuid::new_v4().to_string()
    }
    fn random(&self) -> f64 {
        #[cfg(test)]
        if self.test_base.is_some() {
            return 0.25;
        }
        let bytes = uuid::Uuid::new_v4().into_bytes();
        // First 48 bits are unaffected by the UUID version/variant bits.
        let number = bytes[..6]
            .iter()
            .fold(0u64, |n, b| (n << 8) | u64::from(*b));
        number as f64 / 281_474_976_710_656.0
    }
    fn rand_suffix(&self) -> String {
        format!(
            "{}_{}",
            (self.random() * 99_999.0).floor() as u64 + 1,
            (self.random() * 9_999_999_999.0).floor() as u64 + 1
        )
    }
    fn url(&self, web: bool, path: &str, pairs: &[(String, String)]) -> Url {
        let mut url =
            Url::parse(&format!("{}{path}", if web { WEB } else { APP })).expect("fixed endpoint");
        #[cfg(test)]
        if let Some(base) = &self.test_base {
            url.set_scheme(base.scheme()).expect("test scheme");
            url.set_host(base.host_str()).expect("test host");
            url.set_port(base.port()).expect("test port");
        }
        if !pairs.is_empty() {
            url.query_pairs_mut()
                .extend_pairs(pairs.iter().map(|(k, v)| (k, v)));
        }
        url
    }
    fn url_raw(&self, path: &str, query: &str) -> Url {
        let mut url = self.url(false, path, &[]);
        // These two deployed methods concatenate query values without form encoding.
        // Parse only after concatenation, matching fetch's URL fragment handling.
        let raw = format!("{}{query}", url.as_str().to_owned() + "?");
        url = Url::parse(&raw).expect("fixed origin plus query");
        url
    }
    fn request(&self, method: Method, url: Url) -> RequestBuilder {
        self.client
            .request(method, url)
            .timeout(Duration::from_secs(20))
            .header("Accept", ACCEPT)
    }
    fn session_header(&self, request: RequestBuilder) -> RequestBuilder {
        if self.session_key.is_empty() {
            request
        } else {
            request.header("sessionKey", &self.session_key)
        }
    }
    fn app_headers(&self, request: RequestBuilder, path: &str) -> Result<RequestBuilder> {
        let date = httpdate::fmt_http_date(self.now());
        let text = cloud189::app_signing_text(&self.session_key, "GET", path, &date);
        let signature = cloud189::hmac_sha1(&self.session_secret, &text)
            .map_err(|_| error("app_signature", "empty signing key"))?;
        Ok(request
            .header("Date", date)
            .header("SessionKey", &self.session_key)
            .header("Signature", signature)
            .header("user-agent", "desktop")
            .header("X-Request-ID", self.uuid()))
    }
    async fn send(&self, request: RequestBuilder, operation: &'static str) -> Result<Response> {
        request.send().await.map_err(|e| {
            error(
                operation,
                if e.is_timeout() {
                    "request timeout"
                } else if e.is_builder() {
                    "invalid request/header"
                } else {
                    "transport failure"
                },
            )
        })
    }
    async fn json(&self, mut response: Response, operation: &'static str) -> Result<Value> {
        // Fetch in deployment does not reject non-2xx status; valid API JSON is
        // still interpreted. HTML/invalid JSON is an explicit error, not null.
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| error(operation, "response body failure"))?
        {
            if body.len().saturating_add(chunk.len()) > MAX_JSON {
                return Err(error(operation, "JSON body too large"));
            }
            body.extend_from_slice(&chunk);
        }
        let text =
            std::str::from_utf8(&body).map_err(|_| error(operation, "invalid UTF-8 JSON"))?;
        parse_json_safe(text).map_err(|_| error(operation, "invalid JSON"))
    }
    fn capture(
        &mut self,
        response: &Response,
        replace_share: bool,
        save_if_empty: bool,
        login: bool,
    ) {
        let raw: Vec<String> = response
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok().map(str::to_owned))
            .collect();
        let cookies = extract_cookies(&raw, self.now());
        if cookies.is_empty() {
            return;
        }
        if replace_share || (save_if_empty && self.share_cookies.is_empty()) {
            self.share_cookies = cookies.join("; ");
        }
        if login
            && self.cookie_login_user.is_empty()
            && let Some(cookie) = cookies.iter().find(|c| c.starts_with("COOKIE_LOGIN_USER="))
        {
            // Deployed getShareInfo/checkAccessCode uses split('=')[1].
            self.cookie_login_user = cookie.split('=').nth(1).unwrap_or("").to_owned();
        }
    }

    pub async fn get_share_info(&mut self, share_code: &str) -> Result<Option<Value>> {
        let operation = "get_share_info";
        let query = format!("shareCode={share_code}&noCache={}", self.random());
        let request = self
            .session_header(self.request(
                Method::GET,
                self.url_raw("/open/share/getShareInfoByCodeV2.action", &query),
            ))
            .header("Referer", "https://cloud.189.cn/")
            .header("User-Agent", UA);
        let response = self.send(request, operation).await?;
        self.capture(&response, false, false, true);
        let data = self.json(response, operation).await?;
        let info = if data.get("res_code") == Some(&json!(0)) && truthy(&data["fileId"]) {
            &data
        } else if truthy(&data["fileInfoAO"]["fileId"]) {
            &data["fileInfoAO"]
        } else {
            return Ok(None);
        };
        Ok(Some(json!({
            "fileId": js_string(&info["fileId"]),
            "fileName": fallback(&[&info["fileName"], &info["name"]], json!("未知文件名")),
            "fileSize": fallback(&[&info["fileSize"], &info["size"]], json!(0)),
            "fileType": fallback(&[&info["fileType"]], json!("")),
            "isFolder": loose_one(&info["isFolder"]) || loose_one(&info["isDirectory"]),
            "shareId": fallback(&[&info["shareId"]], json!(""))
        })))
    }

    pub async fn check_access_code(&mut self, share_code: &str, password: &str) -> Result<Value> {
        let operation = "check_access_code";
        let body = format!(
            "shareCode={}&accessCode={}&validateCode=",
            encode_component(share_code),
            encode_component(password)
        );
        let request = self
            .session_header(self.request(
                Method::POST,
                self.url(false, "/open/share/checkAccessCode.action", &[]),
            ))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Referer", "https://cloud.189.cn/")
            .header("User-Agent", format!("{UA} Edg/120.0.0.0"))
            .body(body);
        let response = self.send(request, operation).await?;
        self.capture(&response, true, false, true);
        let data = self.json(response, operation).await?;
        Ok(
            if data.get("res_code") == Some(&json!(0)) && truthy(&data["shareId"]) {
                json!({"success": true, "shareId": js_string(&data["shareId"])})
            } else {
                json!({"success": false})
            },
        )
    }

    pub async fn list_share_dir(
        &mut self,
        share_id: &str,
        folder_id: &str,
        password: &str,
        share_mode: &str,
    ) -> Result<Option<Value>> {
        let modes = [share_mode, if share_mode == "1" { "2" } else { "1" }, "3"];
        let mut last_error = None;
        for mode in modes {
            match self.list_once(0, share_id, folder_id, password, mode).await {
                Ok(Some(list)) => return Ok(Some(list)),
                Ok(None) => {}
                Err(e) => last_error = Some(e),
            }
        }
        for variant in [1, 2, 3] {
            if variant == 2 && self.open_access_token.is_empty() {
                continue;
            }
            if variant == 3 && (self.session_key.is_empty() || self.session_secret.is_empty()) {
                continue;
            }
            match self
                .list_once(variant, share_id, folder_id, password, share_mode)
                .await
            {
                Ok(Some(list)) => return Ok(Some(list)),
                Ok(None) => {}
                Err(e) => last_error = Some(e),
            }
        }
        match last_error {
            Some(e) => Err(e),
            None => Ok(None),
        }
    }
    async fn list_once(
        &mut self,
        variant: u8,
        share_id: &str,
        folder_id: &str,
        password: &str,
        mode: &str,
    ) -> Result<Option<Value>> {
        let operation = match variant {
            0 => "list_v1",
            1 => "list_v3",
            2 => "list_open",
            _ => "list_app",
        };
        let mut params = if variant == 0 {
            pairs(&[
                ("noCache", &self.random().to_string()),
                ("pageNum", "1"),
                ("pageSize", "200"),
                ("fileId", folder_id),
                ("shareDirFileId", folder_id),
                ("isFolder", "true"),
                ("shareId", share_id),
                ("shareMode", mode),
                ("iconOption", "5"),
                ("orderBy", "lastOpTime"),
                ("descending", "true"),
                ("accessCode", password),
            ])
        } else {
            pairs(&[
                ("uuid", &self.uuid()),
                ("shareId", share_id),
                ("fileId", folder_id),
                ("shareMode", mode),
                ("isFolder", "true"),
                ("iconOption", "5"),
                ("pageSize", "200"),
                ("pageNum", "1"),
                ("accessCode", password),
            ])
        };
        if variant == 2 {
            let timestamp = self.timestamp();
            let text = cloud189::open_list_signing_text(
                &self.open_access_token,
                &timestamp,
                folder_id,
                share_id,
            );
            params.extend(pairs(&[
                ("accesstoken", &self.open_access_token),
                ("sign-type", "1"),
                ("signature", &cloud189::java_md5(&text)),
                ("timestamp", &timestamp),
            ]));
        }
        if variant == 3 {
            params.extend(self.app_suffix());
        }
        let mut request = self.request(Method::GET, self.url(variant == 0, LIST, &params));
        if variant == 0 {
            request = self
                .session_header(request)
                .header(
                    "Accept-Language",
                    "zh-CN,zh;q=0.9,en;q=0.8,en-GB;q=0.7,en-US;q=0.6",
                )
                .header("Cache-Control", "no-cache")
                .header("Pragma", "no-cache")
                .header("Referer", "https://cloud.189.cn/")
                .header("sign-type", "1")
                .header("User-Agent", UA_V1);
        } else if variant == 3 {
            request = self.app_headers(request, LIST)?;
        }
        let cookie = if variant == 0 && !self.cookie_login_user.is_empty() {
            format!("COOKIE_LOGIN_USER={}", self.cookie_login_user)
        } else if !self.share_cookies.is_empty() {
            self.share_cookies.clone()
        } else if variant == 1 && !self.cookie_login_user.is_empty() {
            format!("COOKIE_LOGIN_USER={}", self.cookie_login_user)
        } else {
            String::new()
        };
        if !cookie.is_empty() {
            request = request.header("Cookie", cookie);
        }
        let response = self.send(request, operation).await?;
        self.capture(&response, false, true, false);
        let data = self.json(response, operation).await?;
        Ok(parse_file_list(&data))
    }
    fn app_suffix(&self) -> Vec<(String, String)> {
        pairs(&[
            ("clientType", "TELEPC"),
            ("version", "6.2"),
            ("channelId", "web_cloud.189.cn"),
            ("rand", &self.rand_suffix()),
        ])
    }

    /// Open first, then App dt=1+shareId, then App dt=3&flag=1. Unlike the
    /// full JS parse orchestrator, this never falls back to saveAndDownload.
    pub async fn download_url(&mut self, file_id: &str, share_id: &str) -> Result<Option<Value>> {
        let mut last_error = None;
        if !self.open_access_token.is_empty() {
            match self.download_open(file_id, share_id).await {
                Ok(Some(url)) => return Ok(Some(json!(url))),
                Ok(None) => {}
                Err(e) => last_error = Some(e),
            }
        }
        if !self.session_key.is_empty() && !self.session_secret.is_empty() {
            for shared in [true, false] {
                if shared && share_id.is_empty() {
                    continue;
                }
                match self.download_app(file_id, share_id, shared).await {
                    Ok(Some(url)) => return Ok(Some(json!(url))),
                    Ok(None) => {}
                    Err(e) => last_error = Some(e),
                }
            }
        }
        match last_error {
            Some(e) => Err(e),
            None => Ok(None),
        }
    }
    pub async fn download_open(&self, file_id: &str, share_id: &str) -> Result<Option<String>> {
        if self.open_access_token.is_empty() {
            return Ok(None);
        }
        let operation = "download_open";
        let timestamp = self.timestamp();
        let text = cloud189::open_download_signing_text(
            &self.open_access_token,
            &timestamp,
            file_id,
            share_id,
        );
        let query = format!("fileId={file_id}&dt=1&shareId={share_id}");
        let request = self
            .request(
                Method::GET,
                self.url_raw("/open/file/getFileDownloadUrl.action", &query),
            )
            .header("accesstoken", &self.open_access_token)
            .header("sign-type", "1")
            .header("signature", cloud189::java_md5(&text))
            .header("timestamp", timestamp)
            .header("origin", "https://h5.cloud.189.cn")
            .header("referer", "https://h5.cloud.189.cn/")
            .header("user-agent", UA_OPEN);
        let data = self
            .json(self.send(request, operation).await?, operation)
            .await?;
        if truthy(&data["errorCode"]) {
            return Ok(None);
        }
        Ok(normalize_download(&data))
    }
    pub async fn download_app(
        &self,
        file_id: &str,
        share_id: &str,
        shared: bool,
    ) -> Result<Option<String>> {
        if self.session_key.is_empty()
            || self.session_secret.is_empty()
            || (shared && share_id.is_empty())
        {
            return Ok(None);
        }
        let operation = "download_app";
        let mut params = self.app_suffix();
        params.extend(if shared {
            pairs(&[("fileId", file_id), ("dt", "1"), ("shareId", share_id)])
        } else {
            pairs(&[("fileId", file_id), ("dt", "3"), ("flag", "1")])
        });
        let request = self.app_headers(
            self.request(Method::GET, self.url(false, DOWNLOAD, &params)),
            DOWNLOAD,
        )?;
        let data = self
            .json(self.send(request, operation).await?, operation)
            .await?;
        Ok(normalize_download(&data))
    }
}

fn pairs(values: &[(&str, &str)]) -> Vec<(String, String)> {
    values
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}
fn fallback(values: &[&Value], default: Value) -> Value {
    values
        .iter()
        .find(|v| truthy(v))
        .map(|v| (*v).clone())
        .unwrap_or(default)
}
fn js_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "null".to_owned(),
        Value::Array(a) => a
            .iter()
            .map(|v| {
                if v.is_null() {
                    String::new()
                } else {
                    js_string(v)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".to_owned(),
        _ => value.to_string(),
    }
}
fn loose_one(value: &Value) -> bool {
    match value {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() == Some(1.0),
        Value::String(s) => s.trim().parse::<f64>() == Ok(1.0),
        Value::Array(a) => loose_one(&json!(js_string(&json!(a)))),
        _ => false,
    }
}
fn parse_file_list(data: &Value) -> Option<Value> {
    if data.get("res_code") != Some(&json!(0)) && data.get("res_code") != Some(&json!("0")) {
        return None;
    }
    let mut output = Vec::new();
    append_lists(&data["fileListAO"], &mut output);
    if output.is_empty() {
        append_lists(data, &mut output);
    }
    Some(Value::Array(output))
}
fn append_lists(data: &Value, output: &mut Vec<Value>) {
    for (key, folder) in [("folderList", true), ("fileList", false)] {
        if let Some(items) = data[key].as_array() {
            for item in items {
                let size = item
                    .get("size")
                    .filter(|v| !v.is_null())
                    .or_else(|| item.get("fileSize").filter(|v| !v.is_null()))
                    .cloned()
                    .unwrap_or(json!(0));
                output.push(json!({
                    "fileId": js_string(&fallback(&[&item["id"], &item["fileId"]], json!(""))),
                    "fileName": fallback(&[&item["name"], &item["fileName"]], json!("")),
                    "fileSize": if folder { json!(0) } else { size },
                    "fileType": fallback(&[&item["fileType"]], json!("")),
                    "isFolder": folder || item["isFolder"] == json!(1) || item["isFolder"] == json!(true),
                    "createDate": fallback(&[&item["createDate"], &item["fileCreateDate"]], json!(""))
                }));
            }
        }
    }
}
fn normalize_download(data: &Value) -> Option<String> {
    let url = data["fileDownloadUrl"].as_str().filter(|s| !s.is_empty())?;
    let url = url.replacen("&amp;", "&", 1);
    Some(if url.starts_with("http") {
        url
    } else {
        format!("https:{url}")
    })
}
fn encode_component(value: &str) -> String {
    let mut encoded = String::new();
    use fmt::Write;
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            encoded.push(byte as char);
        } else {
            write!(&mut encoded, "%{byte:02X}").expect("String write");
        }
    }
    encoded
}

// Same lexical scanner as jxpanParseJsonSafe: quote integer tokens with >=16
// digits before serde_json sees them, including negative IDs and sizes.
pub(crate) fn parse_json_safe(text: &str) -> serde_json::Result<Value> {
    let bytes = text.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            output.push(bytes[i]);
            i += 1;
            while i < bytes.len() {
                let byte = bytes[i];
                output.push(byte);
                i += 1;
                if byte == b'\\' && i < bytes.len() {
                    output.push(bytes[i]);
                    i += 1;
                } else if byte == b'"' {
                    break;
                }
            }
            continue;
        }
        let byte = bytes[i];
        if (byte == b'-' || byte.is_ascii_digit())
            && (i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || b"_\"".contains(&bytes[i - 1])))
        {
            let mut j = i + usize::from(byte == b'-');
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j - i - usize::from(byte == b'-') >= 16 {
                output.push(b'"');
                output.extend_from_slice(&bytes[i..j]);
                output.push(b'"');
            } else {
                output.extend_from_slice(&bytes[i..j]);
            }
            i = j;
            continue;
        }
        output.push(byte);
        i += 1;
    }
    serde_json::from_slice(&output)
}
fn deletion_cookie(cookie: &str, now: SystemTime) -> bool {
    let lower = cookie.to_ascii_lowercase();
    if lower.contains("max-age=0") || lower.contains("max-age=-1") {
        return true;
    }
    for attribute in cookie.split(';').skip(1) {
        if let Some((name, date)) = attribute.trim().split_once('=')
            && name.trim().eq_ignore_ascii_case("expires")
            && let Ok(expires) = httpdate::parse_http_date(date.trim())
        {
            return expires <= now;
        }
    }
    false
}
fn extract_cookies(raw: &[String], now: SystemTime) -> Vec<String> {
    let extract = |cookie: &str| -> Option<String> {
        if deletion_cookie(cookie, now) {
            return None;
        }
        let pair = cookie.split(';').next()?.trim();
        if pair.split('=').nth(1).is_some_and(|s| !s.is_empty()) {
            Some(pair.to_owned())
        } else {
            None
        }
    };
    let mut cookies: Vec<String> = raw.iter().filter_map(|s| extract(s)).collect();
    // Original getSetCookie -> comma-split fallback only if no usable pairs.
    if cookies.is_empty() {
        cookies = raw
            .iter()
            .flat_map(|s| s.split(','))
            .filter_map(extract)
            .collect();
    }
    cookies
}
/// Deployment's ordered cookie-name merge; values may contain '='.
pub fn merge_cookies(existing: &str, incoming: &[String]) -> String {
    let mut cookies: Vec<(String, String)> = Vec::new();
    for cookie in existing
        .split("; ")
        .filter(|s| !s.is_empty())
        .chain(incoming.iter().map(String::as_str))
    {
        let name = cookie.split('=').next().unwrap_or("");
        if name.is_empty() {
            continue;
        }
        if let Some((_, value)) = cookies.iter_mut().find(|(key, _)| key == name) {
            *value = cookie.to_owned();
        } else {
            cookies.push((name.to_owned(), cookie.to_owned()));
        }
    }
    cookies
        .into_iter()
        .map(|(_, value)| value)
        .collect::<Vec<_>>()
        .join("; ")
}
