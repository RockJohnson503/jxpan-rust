//! Cloud189 login/renewal protocol kernel. No storage, global state or live I/O.
//! Transport and time/entropy are injected; request/response types intentionally
//! do not implement Debug because they contain bearer credentials.
use crate::compat::{js_string, truthy};
use serde_json::{Value, json};
use std::future::Future;
use std::sync::LazyLock;
use url::Url;

const OPEN: &str = "https://open.e.189.cn";
const PAGE: &str = "https://open.e.189.cn/api/logbox/separate/web/index.html?appId=8025431004";
const RETURN: &str = "https://m.cloud.189.cn/zhuanti/2020/loginErrorPc/index.html";
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const UA_NEW: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/147.0.0.0 Safari/537.36 Edg/147.0.0.0";
const HTML: &str = "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8";
const ACCEPT: &str = "application/json;charset=UTF-8";

pub struct Request {
    pub url: String,
    pub method: &'static str,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
    /// Deployed fetch semantics. Adapters must restrict any followed redirects
    /// to the allowlisted HTTPS service hosts and never forward cross-host secrets.
    pub redirect: &'static str,
}
impl Request {
    fn get(url: String, headers: Vec<(String, String)>, redirect: &'static str) -> Self {
        Self {
            url,
            method: "GET",
            headers,
            body: None,
            redirect,
        }
    }
    fn post(url: String, headers: Vec<(String, String)>, body: Option<String>) -> Self {
        Self {
            url,
            method: "POST",
            headers,
            body,
            redirect: "follow",
        }
    }
}
pub struct Response {
    pub status: u16,
    /// Preserve repeated Set-Cookie lines, not a comma-coalesced representation.
    pub headers: Vec<(String, String)>,
    pub text: String,
}
impl Response {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    fn json(&self) -> Result<Value, AuthError> {
        if self.text.len() > 2 * 1024 * 1024 {
            return Err(AuthError::Oversized);
        }
        crate::providers::cloud189_client::parse_json_safe(&self.text)
            .map_err(|_| AuthError::InvalidJson)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthError {
    Transport,
    InvalidJson,
    Oversized,
    UnsafeUrl,
    InvalidLoginInfo,
    InvalidQr,
}
impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Cloud189 auth: {self:?}")
    }
}
impl std::error::Error for AuthError {}
pub trait Transport {
    fn fetch(
        &mut self,
        request: Request,
    ) -> impl Future<Output = Result<Response, AuthError>> + Send;
}
/// Dedicated, non-cookie-jar HTTP pool. Constructing it performs no requests.
/// Redirects are manual; implicit fetch-style redirects may only stay on the
/// same service host, so neither a POST body nor credentials can cross hosts.
#[derive(Clone)]
pub struct ReqwestTransport {
    client: reqwest::Client,
}
impl ReqwestTransport {
    pub fn new() -> Result<Self, AuthError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .map_err(|_| AuthError::Transport)?;
        Ok(Self { client })
    }
    /// Caller must disable redirects/cookie jars/default credentials on this pool.
    pub fn with_client(client: reqwest::Client) -> Self {
        Self { client }
    }
}
impl Transport for ReqwestTransport {
    async fn fetch(&mut self, mut request: Request) -> Result<Response, AuthError> {
        for hop in 0..=10 {
            let url = allowed_url(&request.url)?;
            let method = reqwest::Method::from_bytes(request.method.as_bytes())
                .map_err(|_| AuthError::Transport)?;
            let mut builder = self.client.request(method, url.clone());
            for (k, v) in &request.headers {
                builder = builder.header(k, v);
            }
            if let Some(body) = &request.body {
                builder = builder.body(body.clone());
            }
            let mut response = builder.send().await.map_err(|_| AuthError::Transport)?;
            let status = response.status().as_u16();
            if request.redirect == "follow"
                && matches!(status, 301 | 302 | 303 | 307 | 308)
                && let Some(loc) = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
            {
                if hop == 10 {
                    return Err(AuthError::Transport);
                }
                let next = resolve(url.as_str(), loc)?;
                if allowed_url(&next)?.host_str() != url.host_str() {
                    return Err(AuthError::UnsafeUrl);
                }
                request.url = next;
                if status == 303 || ((status == 301 || status == 302) && request.method == "POST") {
                    request.method = "GET";
                    request.body = None;
                    request.headers.retain(|(k, _)| {
                        !k.eq_ignore_ascii_case("content-type")
                            && !k.eq_ignore_ascii_case("content-length")
                    });
                }
                continue;
            }
            if response
                .content_length()
                .is_some_and(|n| n > 2 * 1024 * 1024)
            {
                return Err(AuthError::Oversized);
            }
            let headers = response
                .headers()
                .iter()
                .filter_map(|(k, v)| v.to_str().ok().map(|v| (k.as_str().into(), v.into())))
                .collect();
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| AuthError::Transport)? {
                if bytes.len() + chunk.len() > 2 * 1024 * 1024 {
                    return Err(AuthError::Oversized);
                }
                bytes.extend_from_slice(&chunk);
            }
            let text = String::from_utf8_lossy(&bytes).into_owned();
            return Ok(Response {
                status,
                headers,
                text,
            });
        }
        Err(AuthError::Transport)
    }
}
pub trait Runtime {
    fn now_ms(&self) -> u64;
    fn random(&mut self) -> f64;
    fn uuid(&mut self) -> String;
}
pub struct SystemRuntime;
impl Runtime for SystemRuntime {
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }
    fn random(&mut self) -> f64 {
        let bytes = uuid::Uuid::new_v4().into_bytes();
        let n = bytes[..6]
            .iter()
            .fold(0u64, |n, b| (n << 8) | u64::from(*b));
        n as f64 / 281_474_976_710_656.0
    }
    fn uuid(&mut self) -> String {
        uuid::Uuid::new_v4().to_string()
    }
}
/// Each instance owns its cookie/transport context. A runtime supplied by the
/// caller permits reproducible oracle comparisons without overriding globals.
pub struct AuthKernel<T, R> {
    pub transport: T,
    pub runtime: R,
}
fn headers(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), v.to_string()))
        .collect()
}
fn put(h: &mut Vec<(String, String)>, name: &str, value: &str) {
    if let Some((_, v)) = h.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(name)) {
        *v = value.to_owned();
    } else {
        h.push((name.to_ascii_lowercase(), value.to_owned()));
    }
}
fn js_number(v: &Value) -> Option<f64> {
    match v {
        Value::Null => Some(0.0),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => n.as_f64(),
        Value::Object(_) => None,
        _ => {
            let raw = js_string(v);
            let s = raw.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
            if s.is_empty() {
                return Some(0.0);
            }
            for (prefix, radix) in [
                ("0x", 16),
                ("0X", 16),
                ("0b", 2),
                ("0B", 2),
                ("0o", 8),
                ("0O", 8),
            ] {
                if let Some(s) = s.strip_prefix(prefix) {
                    return u64::from_str_radix(s, radix).ok().map(|v| v as f64);
                }
            }
            s.parse().ok()
        }
    }
}
fn string(v: &Value, k: &str) -> String {
    v.get(k)
        .filter(|v| truthy(v))
        .map(js_string)
        .unwrap_or_default()
}
fn form(pairs: &[(&str, String)]) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs.iter().map(|(k, v)| (*k, v)))
        .finish()
}
/// Unlike the deployed substring host checks, reject lookalikes, userinfo,
/// non-HTTPS URLs and nonstandard ports before emitting any request.
pub fn allowed_url(raw: &str) -> Result<Url, AuthError> {
    let url = Url::parse(raw).map_err(|_| AuthError::UnsafeUrl)?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some_and(|p| p != 443)
        || !matches!(
            url.host_str(),
            Some("cloud.189.cn" | "m.cloud.189.cn" | "api.cloud.189.cn" | "open.e.189.cn")
        )
    {
        return Err(AuthError::UnsafeUrl);
    }
    Ok(url)
}
fn resolve(base: &str, location: &str) -> Result<String, AuthError> {
    let u = allowed_url(base)?
        .join(location)
        .map_err(|_| AuthError::UnsafeUrl)?;
    allowed_url(u.as_str())?;
    Ok(u.into())
}
fn browser() -> Vec<(String, String)> {
    headers(&[
        ("user-agent", UA_NEW),
        ("accept", HTML),
        ("accept-language", "zh-CN,zh;q=0.9,en;q=0.8"),
    ])
}
static COOKIE_EXPIRES_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?i)expires\s*=\s*([^;]+)").unwrap());

/// Faithfully ignores deletion cookies (does not delete existing values).
pub fn extract_cookies(response: &Response, now_ms: u64) -> Vec<String> {
    let mut cookies = Vec::new();
    for (_, line) in response
        .headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("set-cookie"))
    {
        let lower = line.to_ascii_lowercase();
        let expired = COOKIE_EXPIRES_RE
            .captures(line)
            .and_then(|c| httpdate::parse_http_date(&c[1]).ok())
            .is_some_and(|t| t <= std::time::UNIX_EPOCH + std::time::Duration::from_millis(now_ms));
        if lower.contains("max-age=0") || lower.contains("max-age=-1") || expired {
            continue;
        }
        let kv = line.split(';').next().unwrap_or("").trim();
        if kv.split('=').nth(1).is_some_and(|v| !v.is_empty()) {
            cookies.push(kv.to_owned());
        }
    }
    cookies
}
pub fn merge_cookies(existing: &str, added: &[String]) -> String {
    let mut map = serde_json::Map::new();
    for c in existing
        .split("; ")
        .filter(|v| !v.is_empty())
        .chain(added.iter().map(String::as_str))
    {
        let name = c.split('=').next().unwrap_or("");
        // Assignment to Object.prototype.__proto__ with a string has no effect.
        if !name.is_empty() && name != "__proto__" {
            map.insert(name.to_owned(), Value::String(c.to_owned()));
        }
    }
    let index = |s: &str| {
        s.parse::<u32>()
            .ok()
            .filter(|n| *n < u32::MAX && n.to_string() == s)
    };
    let mut numeric = map
        .iter()
        .filter_map(|(k, v)| index(k).map(|i| (i, v)))
        .collect::<Vec<_>>();
    numeric.sort_by_key(|(i, _)| *i);
    numeric
        .into_iter()
        .map(|(_, v)| v)
        .chain(
            map.iter()
                .filter(|(k, _)| index(k).is_none())
                .map(|(_, v)| v),
        )
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("; ")
}
fn capture(html: &str, pattern: &str) -> String {
    regex::Regex::new(pattern)
        .unwrap()
        .captures(html)
        .map(|c| c[1].to_owned())
        .unwrap_or_default()
}
fn iso_date(ms: u64) -> String {
    // Gregorian civil_from_days; Date.toISOString uses UTC, NOT host timezone.
    let seconds = ms / 1000;
    let z = (seconds / 86400) as i64 + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    y += i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        seconds / 3600 % 24,
        seconds / 60 % 60,
        seconds % 60
    )
}
impl<T: Transport, R: Runtime> AuthKernel<T, R> {
    async fn fetch(&mut self, request: Request) -> Result<Response, AuthError> {
        allowed_url(&request.url)?;
        let response = self.transport.fetch(request).await?;
        if response.text.len() > 2 * 1024 * 1024 {
            return Err(AuthError::Oversized);
        }
        Ok(response)
    }
    pub async fn init_qr_login(&mut self) -> Result<Value, AuthError> {
        let mut page = PAGE.to_owned();
        let (mut all, mut cloud, mut open) = (String::new(), String::new(), String::new());
        let mut current = format!(
            "https://cloud.189.cn/api/portal/unifyLoginForPC.action?appId=8025431004&clientType=10020&returnURL={RETURN}&timeStamp={}",
            self.runtime.now_ms()
        );
        let mut html = String::new();
        for _ in 0..5 {
            let mut h = browser();
            let host = allowed_url(&current)?.host_str().unwrap().to_owned();
            let cookie = if host == "open.e.189.cn" {
                &open
            } else {
                &cloud
            };
            if !cookie.is_empty() {
                put(&mut h, "cookie", cookie);
            }
            let resp = match self.fetch(Request::get(current.clone(), h, "manual")).await {
                Ok(r) => r,
                Err(AuthError::UnsafeUrl) => return Err(AuthError::UnsafeUrl),
                Err(_) => break,
            };
            let c = extract_cookies(&resp, self.runtime.now_ms());
            all = merge_cookies(&all, &c);
            if host == "open.e.189.cn" {
                open = merge_cookies(&open, &c);
            } else if host == "cloud.189.cn" || host == "m.cloud.189.cn" {
                cloud = merge_cookies(&cloud, &c);
            }
            if (300..400).contains(&resp.status)
                && let Some(loc) = resp.header("location")
            {
                current = resolve(&current, loc)?;
                continue;
            }
            html = resp.text;
            page = current;
            break;
        }
        let mut lt = capture(&html, r#"(?i)lt\s*=\s*"([A-F0-9]{16,})""#);
        let mut req_id = capture(&html, r#"(?i)reqId\s*=\s*"([^"]{10,})""#);
        let param_id = capture(&html, r#"(?i)paramId\s*=\s*"([^"]*)""#);
        if lt.is_empty()
            && let Ok(resp) = self
                .fetch(Request::get(PAGE.to_owned(), browser(), "manual"))
                .await
        {
            let c = extract_cookies(&resp, self.runtime.now_ms());
            all = merge_cookies(&all, &c);
            open = merge_cookies(&open, &c);
            if let Some(loc) = resp.header("location") {
                let next = resolve(PAGE, loc)?;
                let u = allowed_url(&next)?;
                let next_open = u.host_str() == Some("open.e.189.cn");
                let mut h = browser();
                let scoped = if next_open { &open } else { &cloud };
                if !scoped.is_empty() {
                    put(&mut h, "cookie", scoped);
                }
                if let Ok(r) = self.fetch(Request::get(next.clone(), h, "manual")).await {
                    let c = extract_cookies(&r, self.runtime.now_ms());
                    all = merge_cookies(&all, &c);
                    if next_open {
                        open = merge_cookies(&open, &c);
                    } else {
                        cloud = merge_cookies(&cloud, &c);
                    }
                    for (k, v) in u.query_pairs() {
                        if k == "lt" {
                            lt = v.into_owned();
                        } else if k == "reqId" {
                            req_id = v.into_owned();
                        }
                    }
                    page = next;
                }
            }
        }
        if req_id.is_empty() {
            req_id = (0..32)
                .map(|_| format!("{:x}", (self.runtime.random() * 16.0).floor() as u32))
                .collect();
        }
        let finger =
            ((self.runtime.random() * 9_000_000_000.0).floor() as u64 + 1_000_000_000).to_string();
        let mut h = headers(&[
            ("content-type", "application/x-www-form-urlencoded"),
            ("origin", OPEN),
            ("referer", &page),
            ("accept", "*/*"),
            ("accept-language", "zh-CN,zh;q=0.9,en;q=0.8"),
            ("user-agent", UA_NEW),
            ("user-finger", &finger),
        ]);
        if !lt.is_empty() {
            put(&mut h, "lt", &lt);
        }
        if !req_id.is_empty() {
            put(&mut h, "reqid", &req_id);
        }
        if !open.is_empty() {
            put(&mut h, "cookie", &open);
        }
        let resp = self
            .fetch(Request::post(
                format!("{OPEN}/api/logbox/oauth2/getUUID.do"),
                h,
                Some("appId=8025431004".into()),
            ))
            .await?;
        // initQRLogin uses Response.json, unlike the safe integer parser elsewhere.
        let data: Value = serde_json::from_str(&resp.text).map_err(|_| AuthError::InvalidJson)?;
        let c = extract_cookies(&resp, self.runtime.now_ms());
        all = merge_cookies(&all, &c);
        open = merge_cookies(&open, &c);
        if !truthy(&data["uuid"]) {
            return Err(AuthError::InvalidQr);
        }
        Ok(
            json!({"success":true,"qrUrl":data["uuid"],"encryuuid":string(&data,"encryuuid"),"lt":lt,"reqId":req_id,"paramId":param_id,"loginPageUrl":page,"userFinger":finger,"cloud189Cookies":all,"cloud189DomainCookies":cloud,"openEDomainCookies":open}),
        )
    }
    /// Intentionally emits NO cookies or user-finger header, just like deployment.
    pub async fn check_qr_status(&mut self, qr: &Value) -> Result<Value, AuthError> {
        let now = self.runtime.now_ms();
        let page = if truthy(&qr["loginPageUrl"]) {
            string(qr, "loginPageUrl")
        } else {
            format!("{OPEN}/")
        };
        allowed_url(&page)?;
        let body = form(&[
            ("appId", "8025431004".into()),
            ("clientType", "10020".into()),
            ("returnUrl", RETURN.into()),
            ("paramId", string(qr, "paramId")),
            ("uuid", string(qr, "qrUrl")),
            ("encryuuid", string(qr, "encryuuid")),
            ("date", iso_date(now)),
            ("timeStamp", now.to_string()),
        ]);
        let mut h = headers(&[
            ("content-type", "application/x-www-form-urlencoded"),
            ("origin", OPEN),
            ("referer", &page),
            ("user-agent", UA),
        ]);
        for (field, name) in [("lt", "lt"), ("reqId", "reqid")] {
            let v = string(qr, field);
            if !v.is_empty() {
                put(&mut h, name, &v);
            }
        }
        self.fetch(Request::post(
            format!("{OPEN}/api/logbox/oauth2/qrcodeLoginState.do"),
            h,
            Some(body),
        ))
        .await?
        .json()
    }
    /// Single polling transaction. The returned envelope can contain loginInfo;
    /// persist it privately and redact before exposing status to an unrelated caller.
    /// No persistence is performed by this kernel.
    pub async fn check_and_login(&mut self, qr: &Value) -> Result<Value, AuthError> {
        let result = self.check_qr_status(qr).await?;
        if result["status"] != json!(0) && !truthy(&result["redirectUrl"]) {
            return Ok(poll_state(&result));
        }
        let redirect = string(&result, "redirectUrl");
        if redirect.is_empty() {
            return Err(AuthError::InvalidQr);
        }
        let session = self.exchange_session(&redirect).await?;
        if !truthy(&session["success"]) {
            return Ok(
                json!({"code":500,"msg":format!("Session交换失败: {}",string(&session,"error")),"success":false}),
            );
        }
        let mut user = string(&session, "cookieLoginUser");
        let mut sson = string(&session, "sson");
        if user.is_empty() {
            let cookie = self.get_cookie_login_user(&redirect).await?;
            if truthy(&cookie["cookieLoginUser"]) {
                user = string(&cookie, "cookieLoginUser");
                if truthy(&cookie["sson"]) {
                    sson = string(&cookie, "sson");
                }
            }
        }
        let saved = if truthy(&qr["openEDomainCookies"]) {
            string(qr, "openEDomainCookies")
        } else {
            string(qr, "cloud189Cookies")
        };
        let mut cookies = Vec::new();
        if !user.is_empty() {
            cookies.push(format!("COOKIE_LOGIN_USER={user}"));
        }
        if !sson.is_empty() {
            cookies.push(format!("SSON={sson}"));
        }
        for c in [saved, string(&result, "_pollCookies")] {
            cookies.extend(c.split("; ").filter(|s| !s.is_empty()).map(str::to_owned));
        }
        let mut open = String::new();
        let key = string(&session, "sessionKey");
        if !key.is_empty() {
            // Deployment tolerates Open-token failure and retains App-only login.
            if let Ok(token) = self.get_open_access_token(&key, &cookies.join("; ")).await
                && truthy(&token["success"])
            {
                open = string(&token, "openAccessToken");
            }
        }
        let app = string(&session, "accessToken");
        let now = self.runtime.now_ms() / 1000;
        let mut info = json!({"app_access_token":app,"open_access_token":open,"access_token":if open.is_empty(){&app}else{&open},"cookie_login_user":user,"sson":sson,"login_time":now,"expires_at":now+86400*3650,"has_valid_open_token":!open.is_empty(),"token_source":if open.is_empty(){"app_only"}else{"open_api"}});
        for (from, to) in [
            ("sessionKey", "session_key"),
            ("sessionSecret", "session_secret"),
            ("refreshToken", "refresh_token"),
        ] {
            if let Some(v) = session.get(from) {
                info[to] = v.clone();
            }
        }
        Ok(
            json!({"code":200,"msg":"登录成功","success":true,"data":{"status":"success","loginInfo":info}}),
        )
    }
    pub async fn exchange_session(&mut self, redirect_url: &str) -> Result<Value, AuthError> {
        if !redirect_url.is_empty() {
            allowed_url(redirect_url)?;
        }
        let query = form(&[
            ("clientType", "TELEPC".into()),
            ("version", "7.1.8.0".into()),
            ("channelId", "web_cloud.189.cn".into()),
            ("rand", self.runtime.now_ms().to_string()),
            ("redirectURL", redirect_url.into()),
        ]);
        let h = headers(&[
            ("accept", ACCEPT),
            (
                "user-agent",
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
            ),
            ("x-request-id", &self.runtime.uuid()),
        ]);
        let r = self
            .fetch(Request::post(
                format!("https://api.cloud.189.cn/getSessionForPC.action?{query}"),
                h,
                None,
            ))
            .await?;
        let c = extract_cookies(&r, self.runtime.now_ms());
        let data = r.json()?;
        if data["res_code"] != json!(0) {
            return Ok(
                json!({"success":false,"error":if truthy(&data["res_message"]) {data["res_message"].clone()} else {json!("Session交换失败")}}),
            );
        }
        let mut out = json!({"success":true,"cookieLoginUser":"","sson":""});
        for k in ["sessionKey", "sessionSecret", "accessToken", "refreshToken"] {
            if let Some(v) = data.get(k) {
                out[k] = v.clone();
            }
        }
        for cookie in c {
            // Deployed exchange splits at '=' and discards further segments.
            let mut p = cookie.split('=');
            let name = p.next().unwrap_or("");
            let v = p.next().unwrap_or("");
            if name == "COOKIE_LOGIN_USER" {
                out["cookieLoginUser"] = json!(v);
            } else if name == "SSON" {
                out["sson"] = json!(v);
            }
        }
        Ok(out)
    }
    pub async fn get_open_access_token(
        &mut self,
        key: &str,
        cookies: &str,
    ) -> Result<Value, AuthError> {
        let mut h = headers(&[
            ("appkey", "600100422"),
            ("accept", ACCEPT),
            ("referer", "https://cloud.189.cn/web/main/file/folder/-11"),
        ]);
        if !cookies.is_empty() {
            put(&mut h, "cookie", cookies);
        }
        // Deployment uses raw concatenation, NOT URLSearchParams here.
        let raw = format!(
            "https://cloud.189.cn/api/open/oauth2/getAccessTokenBySsKey.action?noCache={}&sessionKey={key}",
            self.runtime.random()
        );
        let data = self.fetch(Request::get(raw, h, "follow")).await?.json()?;
        if truthy(&data["accessToken"]) {
            Ok(json!({"success":true,"openAccessToken":data["accessToken"]}))
        } else {
            Ok(
                json!({"success":false,"error":data.get("errorCode").filter(|v|truthy(v)).or_else(||data.get("res_message").filter(|v|truthy(v))).cloned().unwrap_or(json!("获取Open API Token失败"))}),
            )
        }
    }
    /// Cookie acquisition, separate from session exchange. Redirects are bounded
    /// and host-scoped; unlike deployment, cross-host cookie forwarding is refused.
    pub async fn get_cookie_login_user(&mut self, redirect_url: &str) -> Result<Value, AuthError> {
        let mut user = String::new();
        let mut sson = String::new();
        for (index, start) in [
            "https://cloud.189.cn/api/portal/loginUrl.action",
            redirect_url,
        ]
        .iter()
        .enumerate()
        {
            if start.is_empty() {
                continue;
            }
            let mut current = start.to_string();
            let mut all = Vec::new();
            let mut cookies_by_host = std::collections::HashMap::<String, String>::new();
            for hop in 0..if index == 0 { 11 } else { 10 } {
                let host = allowed_url(&current)?.host_str().unwrap().to_string();
                let mut h = headers(&[
                    ("accept", HTML),
                    ("user-agent", UA),
                    ("referer", "https://cloud.189.cn/"),
                ]);
                if let Some(c) = cookies_by_host.get(&host) {
                    put(&mut h, "cookie", c);
                }
                let r = match self.fetch(Request::get(current.clone(), h, "manual")).await {
                    Ok(r) => r,
                    Err(_) => break,
                };
                let c = extract_cookies(&r, self.runtime.now_ms());
                let saved = cookies_by_host.entry(host).or_default();
                *saved = merge_cookies(saved, &c);
                all.extend(c);
                for c in &all {
                    if let Some((name, v)) = c.split_once('=') {
                        if name == "COOKIE_LOGIN_USER" && (index == 0 || user.is_empty()) {
                            user = v.into();
                        }
                        if name == "SSON" && (index == 0 || sson.is_empty()) {
                            sson = v.into();
                        }
                    }
                }
                if index == 1 && !user.is_empty() {
                    break;
                }
                // Way 1 follows initial Location irrespective of status.
                if ((index == 0 && hop == 0) || (300..400).contains(&r.status))
                    && let Some(loc) = r.header("location")
                {
                    current = resolve(&current, loc)?;
                    continue;
                }
                break;
            }
            if !user.is_empty() {
                return Ok(json!({"cookieLoginUser":user,"sson":sson}));
            }
        }
        // Deployment discards a stand-alone SSON on total failure.
        Ok(json!({"cookieLoginUser":"","sson":""}))
    }
    /// Renewal is atomic: return a complete replacement only after BOTH upstream
    /// calls succeed. Never mutate input/store partial credentials on failures.
    /// The frozen implementation renews using app_access_token, NOT refresh_token.
    pub async fn refresh_on_demand(&mut self, info: &Value) -> Result<Option<Value>, AuthError> {
        let app = string(info, "app_access_token");
        if app.is_empty() {
            return Ok(None);
        }
        let rand = format!(
            "{}_{}",
            self.runtime.now_ms(),
            (self.runtime.random() * 9_999_999_999.0).floor() as u64
        );
        let query = form(&[
            ("clientType", "TELEPC".into()),
            ("version", "6.2".into()),
            ("channelId", "web_cloud.189.cn".into()),
            ("rand", rand),
            ("appId", "8025431004".into()),
            ("accessToken", app),
        ]);
        let r = self
            .fetch(Request::get(
                format!("https://api.cloud.189.cn/getSessionForPC.action?{query}"),
                headers(&[("accept", ACCEPT), ("user-agent", "desktop")]),
                "follow",
            ))
            .await?;
        let data = r.json()?;
        if r.status != 200
            || data["res_code"] != json!(0)
            || !truthy(&data["sessionKey"])
            || !truthy(&data["sessionSecret"])
        {
            return Ok(None);
        }
        let cookies = [("COOKIE_LOGIN_USER", "cookie_login_user"), ("SSON", "sson")]
            .iter()
            .filter_map(|(name, key)| {
                let v = string(info, key);
                if v.is_empty() {
                    None
                } else {
                    Some(format!("{name}={v}"))
                }
            })
            .collect::<Vec<_>>()
            .join("; ");
        let open = self
            .get_open_access_token(&string(&data, "sessionKey"), &cookies)
            .await?;
        if !truthy(&open["success"]) || !truthy(&open["openAccessToken"]) {
            return Ok(None);
        }
        let mut updated = info.clone();
        if !updated.is_object() {
            return Err(AuthError::InvalidLoginInfo);
        }
        let now = self.runtime.now_ms() / 1000;
        updated["session_key"] = data["sessionKey"].clone();
        updated["session_secret"] = data["sessionSecret"].clone();
        updated["open_access_token"] = open["openAccessToken"].clone();
        updated["access_token"] = open["openAccessToken"].clone();
        updated["has_valid_open_token"] = json!(true);
        updated["login_time"] = json!(now);
        updated["expires_at"] = json!(now + 86400 * 3650);
        updated["last_on_demand_refresh_at"] = json!(now);
        updated["token_source"] = json!("on_demand_refresh");
        Ok(Some(updated))
    }
}
/// Frozen status projection is LOCAL metadata only; it does not assert that
/// upstream accepts the token. Exact expiry boundary: now == expires_at is valid.
pub fn login_status(info: Option<&Value>, now_ms: u64) -> Value {
    let Some(info) = info else {
        return json!({"code":200,"msg":"未登录","success":true,"data":{"logged_in":false,"message":"请先扫码登录或配置AccessToken"}});
    };
    let expired = !truthy(&info["expires_at"])
        || js_number(&info["expires_at"]).is_some_and(|exp| (now_ms / 1000) as f64 > exp);
    let mut data = json!({"logged_in":!expired,"expired":expired,"has_session_key":truthy(&info["session_key"]),"has_open_token":truthy(&info["open_access_token"]),"source":if truthy(&info["source"]) {info["source"].clone()} else {json!("qrcode")}});
    for k in ["login_time", "expires_at"] {
        if let Some(v) = info.get(k) {
            data[k] = v.clone();
        }
    }
    json!({"code":200,"msg":if expired {"登录信息已过期"} else {"已登录"},"success":true,"data":data})
}
/// Additional strict input validation (not present in the frozen status handler).
/// Use before persistence; no credentials are included in errors.
pub fn validate_login_info(info: &Value) -> Result<(), AuthError> {
    if !info.is_object() {
        return Err(AuthError::InvalidLoginInfo);
    }
    for key in [
        "session_key",
        "session_secret",
        "app_access_token",
        "open_access_token",
        "access_token",
        "refresh_token",
        "cookie_login_user",
        "sson",
    ] {
        if let Some(v) = info.get(key) {
            let Some(s) = v.as_str() else {
                return Err(AuthError::InvalidLoginInfo);
            };
            if s.len() > 16384 || s.chars().any(|c| c.is_control()) {
                return Err(AuthError::InvalidLoginInfo);
            }
        }
    }
    if ["app_access_token", "open_access_token", "access_token"]
        .iter()
        .all(|k| string(info, k).is_empty())
    {
        return Err(AuthError::InvalidLoginInfo);
    }
    if string(info, "session_key").is_empty() != string(info, "session_secret").is_empty() {
        return Err(AuthError::InvalidLoginInfo);
    }
    for k in ["login_time", "expires_at"] {
        if info[k].as_u64().is_none_or(|v| v == 0) {
            return Err(AuthError::InvalidLoginInfo);
        }
    }
    if info["expires_at"].as_u64() < info["login_time"].as_u64()
        || (truthy(&info["has_valid_open_token"]) && string(info, "open_access_token").is_empty())
    {
        return Err(AuthError::InvalidLoginInfo);
    }
    Ok(())
}
/// Frozen polling state precedence, independent of local QR/session persistence.
pub fn poll_state(result: &Value) -> Value {
    if result["status"] == json!(0) || truthy(&result["redirectUrl"]) {
        return json!({"state":"exchange"});
    }
    let status = &result["status"];
    if status == &json!(-106) || status == &json!(-1) {
        return json!({"code":200,"msg":"waiting","success":true,"data":{"status":"waiting"}});
    }
    if status == &json!(-11002) {
        return json!({"code":200,"msg":"scanned","success":true,"data":{"status":"scanned"}});
    }
    if status == &json!(-11001) {
        return json!({"code":400,"msg":"二维码已过期","success":false,"data":{"status":"expired"}});
    }
    if result["result"] == json!(-20099) || string(result, "msg").contains("过期") {
        let mut d = json!({"status":"expired"});
        if truthy(&result["result"]) {
            d["error_code"] = result["result"].clone();
        } else if let Some(s) = result.get("status") {
            d["error_code"] = s.clone();
        }
        return json!({"code":400,"msg":if truthy(&result["msg"]) {result["msg"].clone()} else {json!("二维码已过期，请刷新重试")},"success":false,"data":d});
    }
    let mut d = json!({"status":"polling"});
    if let Some(s) = result.get("status") {
        d["raw_status"] = s.clone();
    }
    json!({"code":200,"msg":"polling","success":true,"data":d})
}
