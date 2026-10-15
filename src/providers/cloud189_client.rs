//! Read-only network adapter for the deployed Cloud189Parser. No login, refresh,
//! transfer, deletion, tree cache, or download-body requests are performed here.
use crate::compat::{encode_uri_component, truthy};
use crate::providers::cloud189;
use crate::settings::{UpstreamLimit, UpstreamPermit};
use reqwest::{Client, Method, RequestBuilder, Response};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use url::Url;

#[path = "cloud189_client/cookies.rs"]
mod cookies;
#[path = "cloud189_client/json.rs"]
mod json;

use cookies::extract_cookies;
pub(crate) use json::parse_json_safe;

const APP: &str = "https://api.cloud.189.cn";
const WEB: &str = "https://cloud.189.cn/api";
const ACCEPT: &str = "application/json;charset=UTF-8";
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const UA_V1: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/147.0.0.0 Safari/537.36 Edg/147.0.0.0";
const UA_OPEN: &str = "Mozilla/5.0 (Linux; Android 6.0; Nexus 5 Build/MRA58N) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/134.0.0.0 Mobile Safari/537.36 Edg/134.0.0.0";
const LIST: &str = "/open/share/listShareDir.action";
const DOWNLOAD: &str = "/getFileDownloadUrl.action";
const MAX_JSON: usize = 2 * 1024 * 1024;
const LIST_PAGE_SIZE: usize = 200;
const MAX_LIST_PAGES: usize = 100;
const MAX_LIST_ITEMS: usize = 20_000;
const MAX_LIST_BYTES: usize = 4 * 1024 * 1024;

/// Intentionally contains neither request URLs nor response bodies/credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cloud189Error {
    pub operation: &'static str,
    pub kind: &'static str,
}
impl Cloud189Error {
    /// Safe categories only: caller-visible status never contains supplier data.
    pub fn status_code(&self) -> u16 {
        match self.kind {
            "authentication required" => 401,
            "share unavailable" => 404,
            "access code required" | "access code rejected" => 403,
            kind if kind.ends_with("limit exceeded") || kind == "JSON body too large" => 413,
            _ => 502,
        }
    }
}
impl From<()> for Cloud189Error {
    fn from(_: ()) -> Self {
        error("callback", "upstream failure")
    }
}
impl From<&str> for Cloud189Error {
    fn from(_: &str) -> Self {
        error("callback", "upstream failure")
    }
}
impl From<String> for Cloud189Error {
    fn from(_: String) -> Self {
        error("callback", "upstream failure")
    }
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

struct ListRoute<'a> {
    variant: u8,
    share_id: &'a str,
    folder_id: &'a str,
    password: &'a str,
    mode: &'a str,
}
impl ListRoute<'_> {
    fn operation(&self) -> &'static str {
        match self.variant {
            0 => "list_v1",
            1 => "list_v3",
            2 => "list_open",
            _ => "list_app",
        }
    }
}
struct ListPage {
    items: Vec<Value>,
    total: Option<usize>,
}

struct LimitedResponse {
    response: Response,
    _permit: UpstreamPermit,
}
impl std::ops::Deref for LimitedResponse {
    type Target = Response;
    fn deref(&self) -> &Response {
        &self.response
    }
}
impl std::ops::DerefMut for LimitedResponse {
    fn deref_mut(&mut self) -> &mut Response {
        &mut self.response
    }
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
    limiter: UpstreamLimit,
    auth_rejection: Option<Arc<AtomicBool>>,
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
            limiter: UpstreamLimit::default(),
            auth_rejection: None,
            #[cfg(test)]
            test_base: None,
        }
    }

    /// All providers and concurrent parses share this process-wide I/O budget.
    pub fn with_limit(mut self, limiter: UpstreamLimit) -> Self {
        self.limiter = limiter;
        self
    }
    /// Attach a caller-owned, per-attempt signal; clones share only this attempt.
    pub(crate) fn with_auth_rejection(mut self, rejected: Arc<AtomicBool>) -> Self {
        self.auth_rejection = Some(rejected);
        self
    }

    fn mark_auth_rejection(&self) {
        if let Some(rejected) = &self.auth_rejection {
            rejected.store(true, Ordering::Relaxed);
        }
    }

    pub fn concurrency(&self) -> usize {
        self.limiter.concurrency()
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
    async fn send(
        &self,
        request: RequestBuilder,
        operation: &'static str,
    ) -> Result<LimitedResponse> {
        let permit = self.limiter.acquire().await;
        let response = request.send().await.map_err(|e| {
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
        })?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            self.mark_auth_rejection();
            return Err(error(operation, "authentication required"));
        }
        Ok(LimitedResponse {
            response,
            _permit: permit,
        })
    }
    async fn json(&self, mut response: LimitedResponse, operation: &'static str) -> Result<Value> {
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
        let data = parse_json_safe(text).map_err(|_| error(operation, "invalid JSON"))?;
        if let Some(rejection) = api_rejection(&data, operation)
            && rejection.kind == "authentication required"
        {
            // The compatibility scanner can quote invalid long number tokens.
            // Require the original body to be complete JSON before signalling auth.
            serde_json::from_str::<Value>(text).map_err(|_| error(operation, "invalid JSON"))?;
            self.mark_auth_rejection();
            return Err(rejection);
        }
        Ok(data)
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
            self.cookie_login_user = cookie
                .split_once('=')
                .map(|(_, value)| value)
                .unwrap_or("")
                .to_owned();
        }
    }

    pub async fn get_share_info(&mut self, share_code: &str) -> Result<Option<Value>> {
        let operation = "get_share_info";
        let params = pairs(&[
            ("shareCode", share_code),
            ("noCache", &self.random().to_string()),
        ]);
        let mut request = self
            .session_header(self.request(
                Method::GET,
                self.url(false, "/open/share/getShareInfoByCodeV2.action", &params),
            ))
            .header("Referer", "https://cloud.189.cn/")
            .header("User-Agent", UA);
        // Password checks capture an instance-local share session. Send it only
        // to this fixed metadata endpoint; never depend on a shared cookie jar.
        if !self.share_cookies.is_empty() {
            request = request.header("Cookie", &self.share_cookies);
        }
        let response = self.send(request, operation).await?;
        self.capture(&response, false, false, true);
        let data = self.json(response, operation).await?;
        if let Some(rejection) = api_rejection(&data, operation)
            && rejection.kind != "access code required"
        {
            return Err(rejection);
        }
        let info = if data.get("res_code").is_some_and(zero_code)
            && data.get("fileId").is_some_and(|v| !v.is_null())
        {
            &data
        } else if data["fileInfoAO"]
            .get("fileId")
            .is_some_and(|v| !v.is_null())
        {
            &data["fileInfoAO"]
        } else {
            return Ok(None);
        };
        let id = valid_id(&info["fileId"]).ok_or_else(|| error(operation, "invalid file ID"))?;
        let share_id = match info
            .get("shareId")
            .filter(|v| !v.is_null())
            .or_else(|| data.get("shareId").filter(|v| !v.is_null()))
        {
            None => String::new(),
            Some(value) => valid_id(value).ok_or_else(|| error(operation, "invalid share ID"))?,
        };
        let folder = crate::tree::is_folder(info);
        let size = if folder {
            json!(0)
        } else {
            crate::tree::byte_size(
                info.get("fileSize")
                    .filter(|v| !v.is_null())
                    .or_else(|| info.get("size"))
                    .unwrap_or(&Value::Null),
            )
            .map_err(|_| error(operation, "invalid file size"))?
        };
        let mut normalized = json!({
            "fileId": id,
            "fileName": fallback(&[&info["fileName"], &info["name"]], json!("未知文件名")),
            "fileSize": size,
            "fileType": fallback(&[&info["fileType"]], json!("")),
            "isFolder": folder,
            "shareId": share_id
        });
        if let Some(mode) = info
            .get("shareMode")
            .filter(|v| !v.is_null())
            .or_else(|| data.get("shareMode").filter(|v| !v.is_null()))
        {
            let mode = valid_id(mode).ok_or_else(|| error(operation, "invalid share mode"))?;
            if !matches!(mode.as_str(), "1" | "2" | "3" | "4" | "5") {
                return Err(error(operation, "invalid share mode"));
            }
            normalized["shareMode"] = json!(mode);
        }
        if let Some(required) = info
            .get("needAccessCode")
            .or_else(|| data.get("needAccessCode"))
        {
            match required {
                Value::Bool(required) => normalized["needAccessCode"] = json!(required),
                value if value == &json!(0) || value == &json!("0") => {
                    normalized["needAccessCode"] = json!(false)
                }
                value if value == &json!(1) || value == &json!("1") => {
                    normalized["needAccessCode"] = json!(true)
                }
                _ => {}
            }
        } else if normalized["shareMode"] == "2"
            && !share_id.is_empty()
            && info
                .get("accessCode")
                .is_some_and(|value| value.as_str() == Some(""))
        {
            // Verified public mode-2 shape: there is no password to validate.
            // PanSou can still attach an irrelevant code; checkAccessCode then
            // returns res_code=0, success=false, not a protected-share session.
            normalized["needAccessCode"] = json!(false);
        }
        Ok(Some(normalized))
    }

    pub async fn check_access_code(&mut self, share_code: &str, password: &str) -> Result<Value> {
        let operation = "check_access_code";
        let body = format!(
            "shareCode={}&accessCode={}&validateCode=",
            encode_uri_component(share_code),
            encode_uri_component(password)
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
        if let Some(rejection) = api_rejection(&data, operation)
            && rejection.kind != "access code rejected"
        {
            return Err(rejection);
        }
        Ok(
            if data.get("res_code").is_some_and(zero_code)
                && !api_error(&data)
                && let Some(id) = valid_id(&data["shareId"])
            {
                json!({"success": true, "shareId": id})
            } else {
                json!({"success": false})
            },
        )
    }

    /// Returns only a complete directory. Empty arrays are valid; rejected,
    /// malformed, repeated, inconsistent or resource-limited lists are errors.
    /// `Option` is retained for caller compatibility; success is always `Some`.
    pub async fn list_share_dir(
        &mut self,
        share_id: &str,
        folder_id: &str,
        password: &str,
        share_mode: &str,
    ) -> Result<Option<Value>> {
        let alternate = if share_mode == "1" { "2" } else { "1" };
        let mut last_error = error("list_v1", "list rejected");
        for (variant, mode) in [
            (0, share_mode),
            (0, alternate),
            (0, "3"),
            (1, share_mode),
            (2, share_mode),
            (3, share_mode),
        ] {
            if variant == 2 && self.open_access_token.is_empty() {
                continue;
            }
            if variant == 3 && (self.session_key.is_empty() || self.session_secret.is_empty()) {
                continue;
            }
            let route = ListRoute {
                variant,
                share_id,
                folder_id,
                password,
                mode,
            };
            match self.list_page(&route, 1).await {
                // Once a route accepts page 1, pin it. A later failure must never
                // expose a partial list or mix pages from a different mode/API.
                Ok(first) => return self.list_pages(&route, first).await.map(Some),
                Err(e) => last_error = e,
            }
        }
        Err(last_error)
    }

    async fn list_pages(&mut self, route: &ListRoute<'_>, mut page: ListPage) -> Result<Value> {
        let operation = route.operation();
        let mut output = Vec::new();
        let mut seen = HashSet::new();
        let mut total = None;
        let mut retained_bytes = 0usize;
        for page_num in 1..=MAX_LIST_PAGES {
            if let Some(reported) = page.total {
                if reported > MAX_LIST_ITEMS {
                    return Err(error(operation, "list item limit exceeded"));
                }
                if total.is_some_and(|previous| previous != reported) {
                    return Err(error(operation, "inconsistent list total"));
                }
                total = Some(reported);
            }
            let length = page.items.len();
            if output.len().saturating_add(length) > MAX_LIST_ITEMS {
                return Err(error(operation, "list item limit exceeded"));
            }
            for item in &page.items {
                retained_bytes = retained_bytes.saturating_add(item.to_string().len() + 256);
                if retained_bytes > MAX_LIST_BYTES {
                    return Err(error(operation, "list memory limit exceeded"));
                }
                let id = item["fileId"].as_str().expect("validated list ID");
                if !seen.insert(id.to_owned()) {
                    return Err(error(operation, "repeated list entry/page"));
                }
            }
            output.append(&mut page.items);
            if let Some(expected) = total {
                if output.len() > expected {
                    return Err(error(operation, "inconsistent list total"));
                }
                if output.len() == expected {
                    return Ok(Value::Array(output));
                }
                if length == 0 {
                    return Err(error(operation, "incomplete file list"));
                }
            } else if length < LIST_PAGE_SIZE {
                return Ok(Value::Array(output));
            }
            if page_num == MAX_LIST_PAGES {
                return Err(error(operation, "list page limit exceeded"));
            }
            page = self.list_page(route, page_num + 1).await?;
        }
        unreachable!("bounded page loop returns or errors")
    }

    async fn list_page(&mut self, route: &ListRoute<'_>, page_num: usize) -> Result<ListPage> {
        let ListRoute {
            variant,
            share_id,
            folder_id,
            password,
            mode,
        } = *route;
        let operation = route.operation();
        let page_num = page_num.to_string();
        let page_size = LIST_PAGE_SIZE.to_string();
        let mut params = if variant == 0 {
            pairs(&[
                ("noCache", &self.random().to_string()),
                ("pageNum", &page_num),
                ("pageSize", &page_size),
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
                ("pageSize", &page_size),
                ("pageNum", &page_num),
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
                ("signature", &cloud189::md5(&text)),
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
        if !response.status().is_success() {
            return Err(error(operation, "HTTP failure"));
        }
        self.capture(&response, false, true, false);
        let data = self.json(response, operation).await?;
        parse_file_list(&data, operation)
    }
    fn app_suffix(&self) -> Vec<(String, String)> {
        pairs(&[
            ("clientType", "TELEPC"),
            ("version", "6.2"),
            ("channelId", "web_cloud.189.cn"),
            ("rand", &self.rand_suffix()),
        ])
    }

    /// Shared Open -> App dt=1+shareId only; never account-owned or transfer APIs.
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
            match self.download_app(file_id, share_id, true).await {
                Ok(Some(url)) => return Ok(Some(json!(url))),
                Ok(None) => {}
                Err(e) => last_error = Some(e),
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
        let params = pairs(&[("fileId", file_id), ("dt", "1"), ("shareId", share_id)]);
        let mut request = self
            .request(
                Method::GET,
                self.url(false, "/open/file/getFileDownloadUrl.action", &params),
            )
            .header("accesstoken", &self.open_access_token)
            .header("sign-type", "1")
            .header("signature", cloud189::md5(&text))
            .header("timestamp", timestamp)
            .header("origin", "https://h5.cloud.189.cn")
            .header("referer", "https://h5.cloud.189.cn/")
            .header("user-agent", UA_OPEN);
        if !self.share_cookies.is_empty() {
            request = request.header("Cookie", &self.share_cookies);
        }
        let data = self
            .json(self.send(request, operation).await?, operation)
            .await?;
        if let Some(rejection) = api_rejection(&data, operation) {
            return Err(rejection);
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
            || !shared
            || share_id.is_empty()
        {
            return Ok(None);
        }
        let operation = "download_app";
        let mut params = self.app_suffix();
        params.extend(pairs(&[
            ("fileId", file_id),
            ("dt", "1"),
            ("shareId", share_id),
        ]));
        let mut request = self.app_headers(
            self.request(Method::GET, self.url(false, DOWNLOAD, &params)),
            DOWNLOAD,
        )?;
        if !self.share_cookies.is_empty() {
            request = request.header("Cookie", &self.share_cookies);
        }
        let data = self
            .json(self.send(request, operation).await?, operation)
            .await?;
        if let Some(rejection) = api_rejection(&data, operation) {
            return Err(rejection);
        }
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
fn valid_id(value: &Value) -> Option<String> {
    match value {
        Value::String(id) if !id.trim().is_empty() => Some(id.clone()),
        Value::Number(id) if id.is_u64() || id.is_i64() => Some(id.to_string()),
        _ => None,
    }
}
fn zero_code(value: &Value) -> bool {
    value == &json!(0) || value == &json!("0")
}
/// Map only known business codes; never echo arbitrary supplier text.
fn api_rejection(data: &Value, operation: &'static str) -> Option<Cloud189Error> {
    for key in ["errorCode", "res_code"] {
        let kind = match data.get(key).and_then(Value::as_str).unwrap_or("") {
            "InvalidSessionKey"
            | "SessionKeyExpired"
            | "InvalidAccessToken"
            | "AccessTokenExpired"
            | "AccessTokenHasExpired"
            | "AccessTokenNotFound"
            | "InvalidUserSession"
            | "UserNotLogin"
            | "NotLogin"
            | "InvalidCookie" => "authentication required",
            "ShareNotFound"
            | "ShareInfoNotFound"
            | "ShareNotFoundError"
            | "ShareExpired"
            | "ShareAuditFailed"
            | "ShareNotFoundOrExpire"
            | "FileNotFound"
            | "FileNotFoundError"
            | "FileNotExists" => "share unavailable",
            "ShareAccessCodeNeeded" | "ShareAccessCodeRequired" => "access code required",
            "ShareAccessCodeError" | "InvalidAccessCode" => "access code rejected",
            _ => continue,
        };
        return Some(error(operation, kind));
    }
    None
}
fn api_error(data: &Value) -> bool {
    data.get("res_code").is_some_and(|v| !zero_code(v))
        || data
            .get("errorCode")
            .is_some_and(|v| truthy(v) && !zero_code(v))
}
fn parse_file_list(data: &Value, operation: &'static str) -> Result<ListPage> {
    if !data.get("res_code").is_some_and(zero_code) || api_error(data) {
        return Err(error(operation, "list rejected"));
    }
    let nested = data.get("fileListAO");
    let source = match nested {
        Some(value) if value.is_object() => {
            if ["folderList", "fileList", "count", "totalCount", "total"]
                .iter()
                .any(|key| value.get(*key).is_some())
            {
                value
            } else {
                data
            }
        }
        Some(value) if !value.is_null() => return Err(error(operation, "invalid file list")),
        _ => data,
    };
    let mut output = Vec::new();
    let mut has_list = false;
    for (key, folder) in [("folderList", true), ("fileList", false)] {
        let Some(value) = source.get(key) else {
            continue;
        };
        has_list = true;
        let items = value
            .as_array()
            .ok_or_else(|| error(operation, "invalid file list"))?;
        for item in items {
            if !item.is_object() {
                return Err(error(operation, "invalid list entry"));
            }
            let id = item
                .get("id")
                .filter(|v| !v.is_null())
                .or_else(|| item.get("fileId"))
                .and_then(valid_id)
                .ok_or_else(|| error(operation, "invalid list entry"))?;
            let folder = folder || crate::tree::is_folder(item);
            // Provider folder size placeholders are not file byte sizes. Keep
            // strict, precision-preserving checks for ordinary files only.
            let size = if folder {
                json!(0)
            } else {
                crate::tree::byte_size(
                    item.get("size")
                        .filter(|v| !v.is_null())
                        .or_else(|| item.get("fileSize").filter(|v| !v.is_null()))
                        .unwrap_or(&Value::Null),
                )
                .map_err(|_| error(operation, "invalid file size"))?
            };
            output.push(json!({
                "fileId": id,
                "fileName": fallback(&[&item["name"], &item["fileName"]], json!("")),
                "fileSize": size,
                "fileType": fallback(&[&item["fileType"]], json!("")),
                "isFolder": folder,
                "createDate": fallback(&[&item["createDate"], &item["fileCreateDate"]], json!(""))
            }));
        }
    }
    // Empty arrays are authoritative: never replace an explicit nested empty
    // directory with unrelated top-level data. Counts may live in either shape.
    let mut total = None;
    for object in [source, data] {
        for key in ["count", "totalCount", "total"] {
            let Some(value) = object.get(key).filter(|v| !v.is_null()) else {
                continue;
            };
            let count = value
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .or_else(|| value.as_str().and_then(|s| s.parse::<usize>().ok()))
                .ok_or_else(|| error(operation, "invalid list total"))?;
            if total.is_some_and(|previous| previous != count) {
                return Err(error(operation, "inconsistent list total"));
            }
            total = Some(count);
        }
    }
    if !has_list {
        // Real v3 empty folders can omit both arrays while explicitly reporting
        // count=0. Missing arrays alone (or an unknown/nonzero/conflicting total)
        // never prove emptiness, and malformed arrays still fail above.
        if total != Some(0) {
            return Err(error(operation, "missing file list"));
        }
        if source
            .get("fileListSize")
            .filter(|value| !value.is_null())
            .is_some_and(|value| !zero_code(value))
        {
            return Err(error(operation, "inconsistent empty file list"));
        }
    }
    Ok(ListPage {
        items: output,
        total,
    })
}
fn normalize_download(data: &Value) -> Option<String> {
    if api_error(data) {
        return None;
    }
    let value = data["fileDownloadUrl"].as_str()?;
    let value = value.replace("&amp;", "&");
    if value.is_empty()
        || value
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '\\')
    {
        return None;
    }
    let url = if value.starts_with("//") {
        if value.starts_with("///") {
            return None;
        }
        format!("https:{value}")
    } else {
        let lower = value.to_ascii_lowercase();
        let authority = if lower.starts_with("https://") {
            &value[8..]
        } else if lower.starts_with("http://") {
            &value[7..]
        } else {
            return None;
        };
        if authority.starts_with('/') {
            return None;
        }
        value
    };
    let parsed = Url::parse(&url).ok()?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return None;
    }
    // Preserve the signed path/query verbatim rather than reserializing Url.
    Some(url)
}

/// Deployment's ordered cookie-name merge; values may contain '='.
pub fn merge_cookies(existing: &str, incoming: &[String]) -> String {
    let mut cookies: Vec<(String, String)> = Vec::new();
    for cookie in existing
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .chain(incoming.iter().map(String::as_str))
    {
        let Some((name, _)) = cookie.split_once('=') else {
            continue;
        };
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

#[cfg(test)]
mod tests {
    use super::*;

    async fn loopback_response(
        status: u16,
        body: String,
    ) -> (Cloud189Client, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/", listener.local_addr().unwrap());
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
            Cloud189Client::for_loopback_test(client, json!({}), &base),
            server,
        )
    }

    #[tokio::test]
    async fn json_auth_whitelist_marks_rejection_at_decode_boundary() {
        for key in ["errorCode", "res_code"] {
            for code in [
                "InvalidSessionKey",
                "SessionKeyExpired",
                "InvalidAccessToken",
                "AccessTokenExpired",
                "AccessTokenHasExpired",
                "AccessTokenNotFound",
                "InvalidUserSession",
                "UserNotLogin",
                "NotLogin",
                "InvalidCookie",
            ] {
                let (client, server) =
                    loopback_response(200, json!({(key): code}).to_string()).await;
                let rejected = Arc::new(AtomicBool::new(false));
                let mut client = client.with_auth_rejection(rejected.clone());
                let error = client.get_share_info("share").await.unwrap_err();
                assert_eq!(error.kind, "authentication required", "{key}: {code}");
                assert!(rejected.load(Ordering::Relaxed), "{key}: {code}");
                server.abort();
            }
        }
    }

    #[tokio::test]
    async fn non_auth_json_and_malformed_bodies_do_not_mark_rejection() {
        for body in [
            r#"{"errorCode":"ShareAccessCodeNeeded"}"#,
            r#"{"errorCode":"InvalidAccessCode"}"#,
            r#"{"res_code":"ShareNotFound"}"#,
            r#"{"res_code":"ShareExpired"}"#,
            r#"{"errorCode":"InvalidSession"}"#,
            r#"{"errorCode":401}"#,
            r#"{"res_code":"UNKNOWN","message":"InvalidSessionKey"}"#,
            r#"{"message":"authentication required"}"#,
            r#"{"errorCode":"InvalidSessionKey""#,
            r#"{"errorCode":"InvalidSessionKey"} trailing"#,
            r#"{"errorCode":"InvalidSessionKey","id":0000000000000000}"#,
            "<html>InvalidSessionKey</html>",
        ] {
            let (client, server) = loopback_response(200, body.to_owned()).await;
            let rejected = Arc::new(AtomicBool::new(false));
            let client = client.with_auth_rejection(rejected.clone());
            let response = client
                .send(
                    client.request(Method::GET, client.url(false, LIST, &[])),
                    "test",
                )
                .await
                .unwrap();
            let result = client.json(response, "test").await;
            match serde_json::from_str::<Value>(body) {
                Ok(expected) => assert_eq!(result.unwrap(), expected),
                Err(_) => assert_eq!(result.unwrap_err().kind, "invalid JSON"),
            }
            assert!(!rejected.load(Ordering::Relaxed), "{body}");
            server.abort();
        }
    }

    #[tokio::test]
    async fn list_http_401_marks_but_other_http_failures_do_not() {
        for status in [401, 403, 429, 500] {
            let (client, server) = loopback_response(status, "not JSON".into()).await;
            let rejected = Arc::new(AtomicBool::new(false));
            let mut client = client.with_auth_rejection(rejected.clone());
            let error = client
                .list_share_dir("share", "folder", "", "1")
                .await
                .unwrap_err();
            assert_eq!(
                error.kind,
                if status == 401 {
                    "authentication required"
                } else {
                    "HTTP failure"
                }
            );
            assert_eq!(rejected.load(Ordering::Relaxed), status == 401);
            server.abort();
        }
    }

    #[tokio::test]
    async fn auth_rejection_clones_share_only_the_attached_attempt() {
        let (client, server) = loopback_response(401, String::new()).await;
        assert!(client.auth_rejection.is_none());
        let first = Arc::new(AtomicBool::new(false));
        let second = Arc::new(AtomicBool::new(false));
        let attempt = client.with_auth_rejection(first.clone());
        let mut cloned = attempt.clone();
        let mut next_attempt = attempt.clone().with_auth_rejection(second.clone());
        assert!(Arc::ptr_eq(
            attempt.auth_rejection.as_ref().unwrap(),
            cloned.auth_rejection.as_ref().unwrap()
        ));
        assert!(cloned.get_share_info("share").await.is_err());
        assert!(first.load(Ordering::Relaxed));
        assert!(!second.load(Ordering::Relaxed));
        assert!(next_attempt.get_share_info("share").await.is_err());
        assert!(second.load(Ordering::Relaxed));
        server.abort();
    }

    #[test]
    fn malformed_or_rejected_lists_are_not_empty_success() {
        for (data, expected) in [
            (json!({"res_code":403,"fileList":[]}), "list rejected"),
            (json!({"fileList":[]}), "list rejected"),
            (
                json!({"res_code":0,"errorCode":"InvalidSession","fileList":[]}),
                "list rejected",
            ),
            (json!({"res_code":0}), "missing file list"),
            (json!({"res_code":0,"fileListAO":{}}), "missing file list"),
            (json!({"res_code":0,"fileList":null}), "invalid file list"),
            (json!({"res_code":0,"fileListAO":[]}), "invalid file list"),
            (
                json!({"res_code":0,"fileList":[null]}),
                "invalid list entry",
            ),
            (
                json!({"res_code":0,"fileList":[{"name":"no ID"}]}),
                "invalid list entry",
            ),
            (
                json!({"res_code":0,"fileList":[],"count":-1}),
                "invalid list total",
            ),
            (
                json!({"res_code":0,"fileList":[],"count":"invalid"}),
                "invalid list total",
            ),
        ] {
            let error = match parse_file_list(&data, "list_v1") {
                Ok(_) => panic!("malformed response must not succeed"),
                Err(error) => error,
            };
            assert_eq!(error.kind, expected);
        }
    }

    #[test]
    fn list_shapes_counts_and_directory_flags_are_normalized() {
        for key in ["count", "totalCount", "total"] {
            for nested in [true, false] {
                let mut source = json!({"fileList":[
                    {"id":"string-folder","isFolder":"1"},
                    {"id":"number-folder","isFolder":1},
                    {"id":"bool-folder","isFolder":true},
                    {"id":"directory-folder","isDirectory":"1"},
                    {"id":"file","isFolder":"0","size":0,"fileSize":99}
                ]});
                source[key] = json!("5");
                let mut data = if nested {
                    json!({"fileListAO":source})
                } else {
                    source
                };
                data["res_code"] = json!("0");
                let page = parse_file_list(&data, "list_v1").unwrap();
                assert_eq!(page.total, Some(5));
                assert_eq!(page.items.len(), 5);
                for item in &page.items[..4] {
                    assert_eq!(item["isFolder"], true);
                }
                assert_eq!(page.items[4]["isFolder"], false);
                assert_eq!(page.items[4]["fileSize"], 0);
            }
        }
        let page = parse_file_list(
            &json!({"res_code":0,"fileListAO":{"fileList":[]},
            "fileList":[{"id":"must-not-leak"}]}),
            "list_v1",
        )
        .unwrap();
        assert!(page.items.is_empty());
        let top = parse_file_list(
            &json!({"res_code":0,"fileListAO":{},"folderList":[{"id":"folder"}],
            "count":1}),
            "list_v1",
        )
        .unwrap();
        assert_eq!(top.items[0]["isFolder"], true);
        assert_eq!(top.total, Some(1));
    }

    #[test]
    fn download_urls_require_valid_http_authority_and_decode_every_amp() {
        for (input, expected) in [
            (
                "https://offline.invalid/file?a=1&amp;b=2&amp;c=3",
                "https://offline.invalid/file?a=1&b=2&c=3",
            ),
            (
                "//offline.invalid/file?signature=a%2Bb%3D&amp;x=2&amp;y=3",
                "https://offline.invalid/file?signature=a%2Bb%3D&x=2&y=3",
            ),
            ("http://offline.invalid/file", "http://offline.invalid/file"),
            (
                "HTTPS://offline.invalid/file",
                "HTTPS://offline.invalid/file",
            ),
            ("https://[::1]:443/file", "https://[::1]:443/file"),
        ] {
            assert_eq!(
                normalize_download(&json!({"fileDownloadUrl":input})).as_deref(),
                Some(expected)
            );
        }
        for input in [
            "",
            "garbage",
            "/relative",
            "https:offline.invalid",
            "httpx://offline.invalid",
            "ftp://offline.invalid/file",
            "javascript:alert(1)",
            "data:text/plain,secret",
            "http://",
            "///offline.invalid/file",
            "https:///offline.invalid/file",
            "https://user:pass@offline.invalid",
            "https://offline.invalid:99999/file",
            "https://offline.invalid\\file",
            "https://offline.invalid/\nfile",
            "https://offline.invalid/space file",
            " https://offline.invalid/file",
        ] {
            assert_eq!(
                normalize_download(&json!({"fileDownloadUrl":input})),
                None,
                "invalid URL: {input:?}"
            );
        }
        for data in [
            json!({"res_code":403,"fileDownloadUrl":"https://offline.invalid/file"}),
            json!({"errorCode":"InvalidSignature","fileDownloadUrl":"https://offline.invalid/file"}),
        ] {
            assert_eq!(normalize_download(&data), None);
        }
        assert!(
            normalize_download(
                &json!({"errorCode":"0","fileDownloadUrl":"//offline.invalid/file"})
            )
            .is_some()
        );
    }

    #[test]
    fn cookie_expiry_is_attribute_based_and_values_preserve_equals() {
        let now = UNIX_EPOCH + Duration::from_millis(1_700_000_000_123);
        let raw: Vec<_> = [
            "COOKIE_LOGIN_USER===; Path=/",
            "KEEP=xmax-age=0==; Max-Age=10; Expires=Thu, 01 Jan 1970 00:00:00 GMT",
            "ZERO=x; Max-Age=0",
            "NEG=x; Max-Age=-2",
            "OLD=x; Expires=Thu, 01 Jan 1970 00:00:00 GMT, LIVE=a=b==; Path=/",
            "EMPTY=; Path=/",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        assert_eq!(
            extract_cookies(&raw, now),
            ["COOKIE_LOGIN_USER===", "KEEP=xmax-age=0==", "LIVE=a=b=="]
        );
        assert_eq!(
            merge_cookies("A=old;B=b==; C=c", &["A=new==".to_owned()]),
            "A=new==; B=b==; C=c"
        );
    }
}
