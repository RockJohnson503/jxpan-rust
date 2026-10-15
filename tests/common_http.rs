//! Isolated public-router regressions: loopback HTTP and a disposable SQLite database.
//! History uses unmigrated URLs; shortlinks use a disabled provider. No upstream I/O.
use base64::{Engine, engine::general_purpose::STANDARD};
use jxpan_rust::{
    http::{self, AppState},
    storage::{Storage, StorageError},
};
use reqwest::{Client, RequestBuilder, Response, StatusCode, header};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{net::TcpListener, task::JoinHandle};

const USER: &str = "common-http-admin";
const PASSWORD: &str = "common-http-test-password";
const CLOUD_TOKEN: &str = "common-http-cloud-secret";
const MOBILE_PLAIN: &str = "test:00000000000:common-http-mobile-secret";
const SHARE: &str = "https://yun.139.com/shareweb/#/w/i/abcdefghijkl";
const BOUNDARY: &str = "common-http-text-boundary";

struct TestDb(PathBuf);
impl TestDb {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("jxpan-common-http-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).expect("create isolated test directory");
        Self(directory)
    }
    async fn open(&self) -> Storage {
        Storage::open(self.0.join("state.sqlite"), 32)
            .await
            .expect("open test SQLite")
    }
}
impl Drop for TestDb {
    fn drop(&mut self) {
        // Only remove the unique directory this fixture created, never a configured DB.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Server {
    origin: String,
    client: Client,
    storage: Storage,
    task: JoinHandle<()>,
}
impl Server {
    async fn start(db: &TestDb) -> Self {
        let storage = db.open().await;
        // Explicit logout markers prevent inherited credential environment variables
        // from seeding tests; existing saves/markers survive fixture restarts.
        for key in ["c189_login_default", "mcloud_login_default"] {
            if storage
                .get(key)
                .await
                .expect("read test auth marker")
                .is_none()
            {
                storage
                    .put(key, json!({"_jxpan_logged_out":true}), None)
                    .await
                    .expect("seed test auth marker");
            }
        }
        let state = AppState::new(
            storage.clone(),
            json!({"mcloud":{"enabled":false,"userAgent":"common-http-test"}}),
            USER,
            PASSWORD,
        )
        .await
        .expect("construct isolated application");
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let origin = format!(
            "http://{}",
            listener.local_addr().expect("loopback address")
        );
        let task = tokio::spawn(async move {
            axum::serve(listener, http::router(state))
                .await
                .expect("serve test router");
        });
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5))
            .build()
            .expect("build loopback client");
        Self {
            origin,
            client,
            storage,
            task,
        }
    }
    fn get(&self, path: &str) -> RequestBuilder {
        self.client.get(format!("{}{path}", self.origin))
    }
    fn post(&self, path: &str) -> RequestBuilder {
        self.client.post(format!("{}{path}", self.origin))
    }
    fn admin_get(&self, cookie: &str, fields: &[(&str, &str)]) -> RequestBuilder {
        self.get("/admin")
            .header(header::COOKIE, cookie)
            .query(fields)
    }
    fn admin_post(&self, cookie: &str, fields: &[(&str, &str)]) -> RequestBuilder {
        self.post("/admin")
            .header(header::COOKIE, cookie)
            .form(fields)
    }
    fn target(&self, fields: &[(&str, &str)]) -> String {
        let mut target = url::Url::parse(&self.origin).expect("loopback URL");
        target
            .query_pairs_mut()
            .extend_pairs(fields.iter().copied());
        target.into()
    }
    fn multipart(&self, path: &str, fields: &[(&str, &str)]) -> RequestBuilder {
        let mut body = String::new();
        for (name, value) in fields {
            body.push_str(&format!(
                "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
            ));
        }
        body.push_str(&format!("--{BOUNDARY}--\r\n"));
        self.post(path)
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={BOUNDARY}"),
            )
            .body(body)
    }
    async fn login(&self, path: &str) -> String {
        let response = send(
            self.post(path)
                // Native form navigation from our no-referrer login page.
                .header(header::ORIGIN, "null")
                .header("sec-fetch-site", "same-origin")
                .header("sec-fetch-mode", "navigate")
                .header("sec-fetch-dest", "document")
                .form(&[
                    ("action", "login"),
                    ("username", USER),
                    ("password", PASSWORD),
                ]),
            StatusCode::OK,
        )
        .await;
        assert_private(&response);
        let set_cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .expect("login sets cookie")
            .to_str()
            .expect("ASCII cookie");
        for attribute in ["Path=/", "Max-Age=604800", "HttpOnly", "SameSite=Lax"] {
            assert!(
                set_cookie.contains(attribute),
                "required login cookie attribute missing"
            );
        }
        assert!(
            !set_cookie.contains("Secure"),
            "loopback HTTP cookie must be usable"
        );
        let cookie = set_cookie
            .split(';')
            .next()
            .expect("session cookie")
            .to_owned();
        let token = cookie
            .strip_prefix("admin_token=")
            .expect("admin cookie name");
        assert_eq!(token.len(), 44);
        assert!(
            token
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
        );
        let body = response.text().await.expect("login HTML");
        assert!(!body.contains(token), "session token leaked into HTML");
        assert_redacted(&body);
        cookie
    }
    async fn parse(&self, url: &str) {
        let value = json_response(
            self.get("/").query(&[("url", url), ("type", "json")]),
            StatusCode::OK,
        )
        .await;
        assert_eq!(value["code"], 501);
        assert_eq!(value["success"], false);
    }
    async fn close(mut self) {
        self.task.abort();
        let _ = (&mut self.task).await;
        self.storage.close().await.expect("close test SQLite");
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn mobile_authorization() -> String {
    format!("Basic {}", STANDARD.encode(MOBILE_PLAIN))
}
fn assert_redacted(body: &str) {
    for secret in [
        PASSWORD,
        CLOUD_TOKEN,
        MOBILE_PLAIN,
        "common-http-mobile-secret",
        "00000000000",
        &mobile_authorization(),
    ] {
        assert!(
            !body.contains(secret),
            "credential material leaked in HTTP body"
        );
    }
}
fn assert_private(response: &Response) {
    assert_private_policy(response, "no-referrer");
}
fn assert_login(response: &Response) {
    assert_private_policy(response, "same-origin");
}
fn assert_private_policy(response: &Response, referrer_policy: &str) {
    assert!(
        !response
            .headers()
            .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN)
    );
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(response.headers()[header::REFERRER_POLICY], referrer_policy);
    assert_eq!(response.headers()["x-frame-options"], "DENY");
}
fn assert_status_redacted(value: &Value) {
    match value {
        Value::Object(fields) => {
            for (name, value) in fields {
                assert!(
                    !matches!(
                        name.to_ascii_lowercase().as_str(),
                        "authorization"
                            | "auth"
                            | "cookie"
                            | "password"
                            | "access_token"
                            | "open_access_token"
                            | "app_access_token"
                            | "refresh_token"
                            | "session_key"
                            | "session_secret"
                            | "logininfo"
                            | "login_info"
                    ),
                    "private credential field leaked in status"
                );
                assert_status_redacted(value);
            }
        }
        Value::Array(values) => {
            for value in values {
                assert_status_redacted(value);
            }
        }
        _ => {}
    }
}
async fn send(request: RequestBuilder, expected: StatusCode) -> Response {
    let response = request.send().await.expect("loopback HTTP request");
    let actual = response.status();
    if actual != expected {
        let path = response.url().path().to_owned();
        let body = response
            .text()
            .await
            .expect("read unexpected HTTP response");
        assert_redacted(&body);
        // A fixed diagnostic vocabulary avoids echoing arbitrary bodies or credentials.
        let reason = [
            "只能缩短本站解析地址",
            "短链参数无效或包含凭据/管理动作",
            "短链创建必须同源",
            "后台修改必须使用同源 POST",
            "缺少或无效的 record_id",
            "日志保留天数必须是 1–3650 的整数",
        ]
        .into_iter()
        .find(|reason| body.contains(reason))
        .unwrap_or("unexpected response");
        panic!("{path}: HTTP {actual}, expected {expected} ({reason})");
    }
    response
}
async fn json_response(request: RequestBuilder, expected: StatusCode) -> Value {
    let response = send(request, expected).await;
    assert!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .is_some_and(|v| v.as_bytes().starts_with(b"application/json"))
    );
    let body = response.text().await.expect("read JSON response");
    assert_redacted(&body);
    serde_json::from_str(&body).unwrap_or_else(|_| panic!("HTTP response was not valid JSON"))
}
async fn private_json(request: RequestBuilder, expected: StatusCode) -> Value {
    let response = send(request, expected).await;
    assert_private(&response);
    let body = response.text().await.expect("read private JSON response");
    assert_redacted(&body);
    serde_json::from_str(&body).unwrap_or_else(|_| panic!("private response was not valid JSON"))
}
async fn status(server: &Server) -> Value {
    let value = private_json(
        server.get("/").query(&[("action", "login_status")]),
        StatusCode::OK,
    )
    .await;
    assert_status_redacted(&value);
    assert_eq!(value["code"], 200);
    value["data"].clone()
}
async fn short_code(server: &Server, target: &str) -> String {
    let value = json_response(
        server
            .post("/api/create-short")
            .header(header::ORIGIN, &server.origin)
            .json(&json!({"url":target})),
        StatusCode::OK,
    )
    .await;
    assert_eq!(value["success"], true);
    let code = value["shortcode"].as_str().expect("created shortcode");
    assert!(!code.is_empty() && code.bytes().all(|b| b.is_ascii_alphanumeric()));
    code.to_owned()
}

#[tokio::test(flavor = "current_thread")]
async fn pages_login_cookie_and_admin_logout() {
    let db = TestDb::new();
    let server = Server::start(&db).await;
    let response = send(server.get("/"), StatusCode::OK).await;
    assert_eq!(response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
    assert!(
        response.headers()[header::CONTENT_TYPE]
            .as_bytes()
            .starts_with(b"text/html")
    );
    assert!(!response.text().await.expect("home HTML").is_empty());
    let mut login_page = String::new();
    for path in ["/admin", "/admin/"] {
        let response = send(server.get(path), StatusCode::OK).await;
        assert_login(&response);
        assert!(!response.headers().contains_key(header::SET_COOKIE));
        let body = response.text().await.expect("login page");
        assert_redacted(&body);
        assert!(body.contains("请登录"));
        if !login_page.is_empty() {
            assert!(body == login_page, "admin slash routes disagree");
        }
        login_page = body;
        let value = private_json(
            server
                .post(path)
                .header(header::ORIGIN, "https://foreign.invalid")
                .form(&[
                    ("action", "login"),
                    ("username", USER),
                    ("password", PASSWORD),
                ]),
            StatusCode::FORBIDDEN,
        )
        .await;
        assert_eq!(value["code"], 403);
    }
    let cookie = server.login("/admin/").await;
    for path in ["/admin", "/admin/"] {
        let response = send(
            server.get(path).header(header::COOKIE, &cookie),
            StatusCode::OK,
        )
        .await;
        assert_private(&response);
        assert!(response.text().await.expect("admin HTML") != login_page);
    }
    private_json(
        server.admin_get(&cookie, &[("action", "logout")]),
        StatusCode::FORBIDDEN,
    )
    .await;
    private_json(
        server.admin_get(&cookie, &[("action", "get_stats")]),
        StatusCode::OK,
    )
    .await;
    let response = send(
        server.admin_post(&cookie, &[("action", "logout")]),
        StatusCode::OK,
    )
    .await;
    assert_login(&response);
    let expired = response.headers()[header::SET_COOKIE]
        .to_str()
        .expect("logout cookie");
    assert!(
        expired.starts_with("admin_token=;")
            && expired.contains("Max-Age=0")
            && expired.contains("HttpOnly")
    );
    private_json(
        server.admin_get(&cookie, &[("action", "get_stats")]),
        StatusCode::UNAUTHORIZED,
    )
    .await;
    server.close().await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_login_origin_and_fetch_metadata_boundaries() {
    let db = TestDb::new();
    let server = Server::start(&db).await;
    for (origin, site) in [
        ("null", None),
        ("null", Some("same-site")),
        ("null", Some("cross-site")),
        ("null", Some("none")),
        ("null", Some("unrecognized")),
        ("not-an-origin", Some("same-origin")),
        ("https://foreign.invalid", Some("same-origin")),
    ] {
        let mut request = server
            .post("/admin?action=login")
            .header(header::ORIGIN, origin)
            .header("sec-fetch-mode", "navigate")
            .header("sec-fetch-dest", "document")
            .form(&[("username", USER), ("password", PASSWORD)]);
        if let Some(site) = site {
            request = request.header("sec-fetch-site", site);
        }
        let response = send(request, StatusCode::FORBIDDEN).await;
        assert_private(&response);
        assert!(!response.headers().contains_key(header::SET_COOKIE));
    }
    let cookie = server.login("/admin?action=login").await;
    let saved = private_json(
        server
            .admin_post(
                &cookie,
                &[("action", "set_log_settings"), ("log_retention_days", "8")],
            )
            .header(header::ORIGIN, "null")
            .header("sec-fetch-site", "same-origin"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(saved["data"]["log_retention_days"], 8);
    server.close().await;
    // A static same-origin referrer policy also supports ordinary HTTP LAN
    // form navigation, where the browser may omit all Fetch Metadata headers.
    let server = Server::start(&db).await;
    let response = send(
        server
            .post("/admin?action=login")
            .header(header::ORIGIN, &server.origin)
            .form(&[("username", USER), ("password", PASSWORD)]),
        StatusCode::OK,
    )
    .await;
    assert_private(&response);
    assert!(response.headers().contains_key(header::SET_COOKIE));
    server.close().await;
}

#[tokio::test(flavor = "current_thread")]
async fn private_operations_require_login_and_same_origin_post() {
    let db = TestDb::new();
    let server = Server::start(&db).await;
    for action in [
        "get_stats",
        "get_records",
        "search_records",
        "get_success_records",
        "get_failed_records",
        "count_records",
        "get_log_settings",
        "set_log_settings",
        "delete_record",
        "clear_all_records",
        "delete_batch_records",
        "cancel_clear",
        "cloud189_manual_save",
        "mcloud_manual_save",
        "cloud189_logout",
        "mcloud_logout",
        "logout",
    ] {
        let value = private_json(
            server.get("/admin").query(&[("action", action)]),
            StatusCode::UNAUTHORIZED,
        )
        .await;
        assert_eq!(value["code"], 401);
    }
    for action in [
        "cloud189_manual_save",
        "mcloud_manual_save",
        "cloud189_logout",
        "mcloud_logout",
        "cloud189_qrcode",
        "mcloud_qrcode",
        "cloud189_check",
        "mcloud_check",
    ] {
        private_json(
            server.post("/").form(&[("action", action)]),
            StatusCode::FORBIDDEN,
        )
        .await;
        private_json(
            server.post("/admin/").form(&[("action", action)]),
            StatusCode::UNAUTHORIZED,
        )
        .await;
    }
    let cookie = server.login("/admin").await;
    for action in [
        "set_log_settings",
        "delete_record",
        "clear_all_records",
        "delete_batch_records",
        "cancel_clear",
        "cloud189_manual_save",
        "mcloud_logout",
        "logout",
    ] {
        private_json(
            server.admin_get(&cookie, &[("action", action)]),
            StatusCode::FORBIDDEN,
        )
        .await;
        private_json(
            server
                .admin_post(&cookie, &[("action", action)])
                .header(header::ORIGIN, "https://foreign.invalid"),
            StatusCode::FORBIDDEN,
        )
        .await;
    }
    let fields = [("action", "set_log_settings"), ("log_retention_days", "14")];
    for origin in [
        "null",
        "not-an-origin",
        "https://127.0.0.1",
        "http://foreign.invalid",
    ] {
        private_json(
            server
                .admin_post(&cookie, &fields)
                .header(header::ORIGIN, origin),
            StatusCode::FORBIDDEN,
        )
        .await;
    }
    private_json(
        server
            .admin_post(&cookie, &fields)
            .header("sec-fetch-site", "cross-site"),
        StatusCode::FORBIDDEN,
    )
    .await;
    private_json(
        server
            .get("/")
            .header(header::COOKIE, &cookie)
            .query(&[("action", "cloud189_logout")]),
        StatusCode::FORBIDDEN,
    )
    .await;
    private_json(
        server
            .post("/")
            .header(header::COOKIE, &cookie)
            .header(header::ORIGIN, "https://foreign.invalid")
            .form(&[("action", "cloud189_logout")]),
        StatusCode::FORBIDDEN,
    )
    .await;
    let settings = private_json(
        server.admin_get(&cookie, &[("action", "get_log_settings")]),
        StatusCode::OK,
    )
    .await;
    assert_eq!(settings["data"]["log_retention_days"], 7);
    let saved = private_json(
        server
            .admin_post(&cookie, &fields)
            .header(header::ORIGIN, &server.origin),
        StatusCode::OK,
    )
    .await;
    assert_eq!(saved["data"]["log_retention_days"], 14);
    server.close().await;
}

#[tokio::test(flavor = "current_thread")]
async fn auth_manual_save_logout_and_public_status_are_redacted_and_persistent() {
    let db = TestDb::new();
    let server = Server::start(&db).await;
    let cookie = server.login("/admin").await;
    let before = status(&server).await;
    assert_eq!(before["cloud189"]["logged_in"], false);
    assert_eq!(before["mcloud"]["logged_in"], false);
    for fields in [
        vec![("action", "cloud189_manual_save"), ("access_token", "")],
        vec![("action", "cloud189_manual_save"), ("login_info", "{")],
        vec![
            ("action", "mcloud_manual_save"),
            ("authorization", "Basic invalid"),
        ],
    ] {
        let value =
            private_json(server.admin_post(&cookie, &fields), StatusCode::BAD_REQUEST).await;
        assert_eq!(value["code"], 400);
    }
    let cloud = private_json(
        server.post("/").header(header::COOKIE, &cookie).form(&[
            ("action", "cloud189_manual_save"),
            ("access_token", CLOUD_TOKEN),
        ]),
        StatusCode::OK,
    )
    .await;
    assert_eq!(cloud["success"], true);
    let mobile = private_json(
        server.admin_post(
            &cookie,
            &[
                ("action", "mcloud_manual_save"),
                ("authorization", &mobile_authorization()),
            ],
        ),
        StatusCode::OK,
    )
    .await;
    assert_eq!(mobile["success"], true);
    let saved = status(&server).await;
    for provider in ["cloud189", "mcloud"] {
        assert_eq!(saved[provider]["configured"], true);
        assert_eq!(saved[provider]["logged_in"], true);
        assert_eq!(saved[provider]["verified"], false);
    }
    let cloud_status = private_json(
        server.get("/").query(&[("action", "cloud189_status")]),
        StatusCode::OK,
    )
    .await;
    assert_status_redacted(&cloud_status);
    assert_eq!(cloud_status["data"]["configured"], true);
    assert!(
        saved["mcloud"]["account_mask"]
            .as_str()
            .is_some_and(|s| s.contains("****"))
    );
    server.close().await;

    let server = Server::start(&db).await;
    let persisted = status(&server).await;
    assert_eq!(persisted["cloud189"]["logged_in"], true);
    assert_eq!(persisted["mcloud"]["logged_in"], true);
    let admin_status = private_json(
        server.admin_get(&cookie, &[("action", "login_status")]),
        StatusCode::OK,
    )
    .await;
    assert_status_redacted(&admin_status);
    private_json(
        server
            .post("/")
            .header(header::COOKIE, &cookie)
            .form(&[("action", "cloud189_logout")]),
        StatusCode::OK,
    )
    .await;
    private_json(
        server.admin_post(&cookie, &[("action", "mcloud_logout")]),
        StatusCode::OK,
    )
    .await;
    let logged_out = status(&server).await;
    for provider in ["cloud189", "mcloud"] {
        assert_eq!(logged_out[provider]["configured"], false);
        assert_eq!(logged_out[provider]["logged_in"], false);
    }
    server.close().await;
    let server = Server::start(&db).await;
    let persisted = status(&server).await;
    assert_eq!(persisted["cloud189"]["configured"], false);
    assert_eq!(persisted["mcloud"]["configured"], false);
    server.close().await;
}

fn assert_parse_settings(value: &Value, concurrency: usize) {
    assert_eq!(value["code"], 200);
    assert_eq!(value["success"], true);
    assert_eq!(
        value["data"],
        json!({"concurrency":concurrency,"default_concurrency":16,
            "min_concurrency":1,"max_concurrency":16})
    );
}

async fn parse_settings(server: &Server, cookie: &str) -> Value {
    private_json(
        server.admin_get(cookie, &[("action", "get_parse_settings")]),
        StatusCode::OK,
    )
    .await
}

// Raw values and TTL/creation metadata detect changes to sessions, auth and any
// other KV without exposing their contents in assertion diagnostics.
async fn unrelated_parse_settings_kv(
    server: &Server,
) -> Vec<(String, String, Option<i64>, Option<i64>)> {
    server
        .storage
        .with_connection(|db| {
            let mut statement = db
                .prepare("SELECT key,value,expires_at,created_at FROM kv_store WHERE key != 'rust_parse_settings' ORDER BY key")
                .map_err(|_| StorageError::Database)?;
            let rows = statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
                .map_err(|_| StorageError::Database)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|_| StorageError::Database)
        })
        .await
        .expect("snapshot unrelated isolated KV")
}

// Comparing stored rows also checks encrypted payload/search metadata without
// depending on history-list presentation or decrypting private fixture data.
async fn parse_settings_history_rows(
    server: &Server,
) -> Vec<(i64, i64, bool, bool, String, String)> {
    server
        .storage
        .with_connection(|db| {
            let mut statement = db
                .prepare("SELECT id,created_ms,success,cached,search_text,payload FROM parse_records ORDER BY id")
                .map_err(|_| StorageError::Database)?;
            let rows = statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)))
                .map_err(|_| StorageError::Database)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|_| StorageError::Database)
        })
        .await
        .expect("snapshot isolated parse history")
}

#[tokio::test(flavor = "current_thread")]
async fn parse_settings_defaults_and_private_mutation_boundaries() {
    let db = TestDb::new();
    let server = Server::start(&db).await;
    assert!(
        server
            .storage
            .get("rust_parse_settings")
            .await
            .unwrap()
            .is_none()
    );
    for path in ["/admin", "/admin/"] {
        for action in ["get_parse_settings", "set_parse_settings"] {
            let value = private_json(
                server
                    .get(path)
                    .query(&[("action", action), ("concurrency", "8")]),
                StatusCode::UNAUTHORIZED,
            )
            .await;
            assert_eq!(value["code"], 401);
            let value = private_json(
                server
                    .post(path)
                    .header(header::ORIGIN, &server.origin)
                    .form(&[("action", action), ("concurrency", "8")]),
                StatusCode::UNAUTHORIZED,
            )
            .await;
            assert_eq!(value["code"], 401);
        }
    }
    let cookie = server.login("/admin").await;
    let before = unrelated_parse_settings_kv(&server).await;
    assert_parse_settings(&parse_settings(&server, &cookie).await, 16);
    for action in ["get_parse_settings", "set_parse_settings"] {
        for authorized in [false, true] {
            let mut get = server
                .get("/")
                .query(&[("action", action), ("concurrency", "8")]);
            let mut post = server
                .post("/")
                .header(header::ORIGIN, &server.origin)
                .form(&[("action", action), ("concurrency", "8")]);
            if authorized {
                get = get.header(header::COOKIE, &cookie);
                post = post.header(header::COOKIE, &cookie);
            }
            private_json(get, StatusCode::FORBIDDEN).await;
            // Even an authenticated same-origin public request cannot dispatch
            // these admin-only actions (the current public API returns 501).
            private_json(
                post,
                if authorized {
                    StatusCode::NOT_IMPLEMENTED
                } else {
                    StatusCode::FORBIDDEN
                },
            )
            .await;
        }
    }
    for path in ["/admin", "/admin/"] {
        private_json(
            server
                .get(path)
                .header(header::COOKIE, &cookie)
                .header(header::ORIGIN, &server.origin)
                .query(&[("action", "set_parse_settings"), ("concurrency", "8")]),
            StatusCode::FORBIDDEN,
        )
        .await;
        for origin in [
            "https://foreign.invalid",
            "null",
            "not-an-origin",
            "https://127.0.0.1",
        ] {
            private_json(
                server
                    .post(path)
                    .header(header::COOKIE, &cookie)
                    .header(header::ORIGIN, origin)
                    .form(&[("action", "set_parse_settings"), ("concurrency", "8")]),
                StatusCode::FORBIDDEN,
            )
            .await;
        }
        private_json(
            server
                .post(path)
                .header(header::COOKIE, &cookie)
                .header(header::ORIGIN, &server.origin)
                .header("sec-fetch-site", "cross-site")
                .form(&[("action", "set_parse_settings"), ("concurrency", "8")]),
            StatusCode::FORBIDDEN,
        )
        .await;
        assert_parse_settings(
            &private_json(
                server
                    .get(path)
                    .header(header::COOKIE, &cookie)
                    .query(&[("action", "get_parse_settings")]),
                StatusCode::OK,
            )
            .await,
            16,
        );
    }
    assert!(
        server
            .storage
            .get("rust_parse_settings")
            .await
            .unwrap()
            .is_none(),
        "reads and rejected writes must not persist the default"
    );
    assert!(
        before == unrelated_parse_settings_kv(&server).await,
        "access checks changed unrelated KV"
    );
    server.close().await;
}

#[tokio::test(flavor = "current_thread")]
async fn parse_settings_reject_invalid_and_missing_concurrency_without_changes() {
    let db = TestDb::new();
    let server = Server::start(&db).await;
    let cookie = server.login("/admin").await;
    let before = unrelated_parse_settings_kv(&server).await;
    for bad in [
        "",
        "0",
        "17",
        "1.5",
        "-1",
        "four",
        "18446744073709551616",
        "99999999999999999999999999999999999999",
    ] {
        let value = private_json(
            server
                .admin_post(
                    &cookie,
                    &[("action", "set_parse_settings"), ("concurrency", bad)],
                )
                .header(header::ORIGIN, &server.origin),
            StatusCode::BAD_REQUEST,
        )
        .await;
        assert_eq!(value["code"], 400);
        assert_eq!(value["success"], false);
        assert_parse_settings(&parse_settings(&server, &cookie).await, 16);
        assert!(
            server
                .storage
                .get("rust_parse_settings")
                .await
                .unwrap()
                .is_none()
        );
    }
    let value = private_json(
        server
            .admin_post(&cookie, &[("action", "set_parse_settings")])
            .header(header::ORIGIN, &server.origin),
        StatusCode::BAD_REQUEST,
    )
    .await;
    assert_eq!(value["code"], 400);
    assert_parse_settings(&parse_settings(&server, &cookie).await, 16);
    assert!(
        server
            .storage
            .get("rust_parse_settings")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        before == unrelated_parse_settings_kv(&server).await,
        "invalid input changed unrelated KV"
    );
    server.close().await;
}

#[tokio::test(flavor = "current_thread")]
async fn parse_settings_save_read_restart_preserves_session_kv_retention_and_history() {
    let db = TestDb::new();
    let server = Server::start(&db).await;
    let cookie = server.login("/admin").await;
    server
        .storage
        .put(
            "common_http_parse_unrelated",
            json!({"keep":true}),
            Some(604800),
        )
        .await
        .unwrap();
    // Cache-shaped KV must also survive settings changes, including its TTL.
    server
        .storage
        .put(
            "common_http_parse_cache",
            json!({"url":"https://unmigrated.invalid/cached","code":501}),
            Some(604800),
        )
        .await
        .unwrap();
    private_json(
        server
            .admin_post(
                &cookie,
                &[("action", "set_log_settings"), ("log_retention_days", "14")],
            )
            .header(header::ORIGIN, &server.origin),
        StatusCode::OK,
    )
    .await;
    server
        .parse("https://unmigrated.invalid/parse-settings-history")
        .await;
    let before_kv = unrelated_parse_settings_kv(&server).await;
    let before_stats = private_json(
        server.admin_get(&cookie, &[("action", "get_stats")]),
        StatusCode::OK,
    )
    .await;
    let before_history = parse_settings_history_rows(&server).await;
    let before_retention = private_json(
        server.admin_get(&cookie, &[("action", "get_log_settings")]),
        StatusCode::OK,
    )
    .await;
    assert_eq!(before_stats["data"]["total"], 1);
    assert_eq!(before_history.len(), 1);
    assert_eq!(before_retention["data"]["log_retention_days"], 14);
    for concurrency in [1, 16, 7] {
        let input = concurrency.to_string();
        let saved = private_json(
            server
                .admin_post(
                    &cookie,
                    &[("action", "set_parse_settings"), ("concurrency", &input)],
                )
                .header(header::ORIGIN, &server.origin),
            StatusCode::OK,
        )
        .await;
        assert_parse_settings(&saved, concurrency);
        assert_parse_settings(&parse_settings(&server, &cookie).await, concurrency);
        assert_eq!(
            server.storage.get("rust_parse_settings").await.unwrap(),
            Some(json!({"concurrency":concurrency}))
        );
        let expiry: i64 = server
            .storage
            .with_connection(|db| {
                db.query_row(
                    "SELECT expires_at FROM kv_store WHERE key='rust_parse_settings'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|_| StorageError::Database)
            })
            .await
            .unwrap();
        assert_eq!(expiry, 0, "saved concurrency must have no TTL");
        assert!(
            before_kv == unrelated_parse_settings_kv(&server).await,
            "saving concurrency changed unrelated KV/session metadata"
        );
        assert_eq!(
            private_json(
                server.admin_get(&cookie, &[("action", "get_stats")]),
                StatusCode::OK
            )
            .await,
            before_stats
        );
        assert!(
            before_history == parse_settings_history_rows(&server).await,
            "saving concurrency changed stored history"
        );
        assert_eq!(
            private_json(
                server.admin_get(&cookie, &[("action", "get_log_settings")]),
                StatusCode::OK
            )
            .await,
            before_retention
        );
    }
    server.close().await;
    let server = Server::start(&db).await;
    // Reuse the original cookie; do not log in again and replace the session.
    assert_parse_settings(&parse_settings(&server, &cookie).await, 7);
    assert!(
        before_kv == unrelated_parse_settings_kv(&server).await,
        "restart changed unrelated KV/session metadata"
    );
    assert_eq!(
        server.storage.get("rust_parse_settings").await.unwrap(),
        Some(json!({"concurrency":7}))
    );
    assert_eq!(
        private_json(
            server.admin_get(&cookie, &[("action", "get_stats")]),
            StatusCode::OK
        )
        .await,
        before_stats
    );
    assert!(
        before_history == parse_settings_history_rows(&server).await,
        "restart changed stored history"
    );
    assert_eq!(
        private_json(
            server.admin_get(&cookie, &[("action", "get_log_settings")]),
            StatusCode::OK
        )
        .await,
        before_retention
    );
    server.close().await;
}

#[tokio::test(flavor = "current_thread")]
async fn parse_settings_failed_persistence_keeps_memory_and_saved_value() {
    let db = TestDb::new();
    let server = Server::start(&db).await;
    let cookie = server.login("/admin").await;
    let saved = private_json(
        server
            .admin_post(
                &cookie,
                &[("action", "set_parse_settings"), ("concurrency", "7")],
            )
            .header(header::ORIGIN, &server.origin),
        StatusCode::OK,
    )
    .await;
    assert_parse_settings(&saved, 7);
    let before_kv = unrelated_parse_settings_kv(&server).await;
    // Fail only this settings write; auth reads and the HTTP server remain
    // usable, unlike closing the shared SQLite worker before the request.
    server.storage.with_connection(|db| {
        db.execute_batch("CREATE TRIGGER common_http_fail_parse_insert BEFORE INSERT ON kv_store WHEN NEW.key='rust_parse_settings' BEGIN SELECT RAISE(ABORT, 'isolated settings failure'); END;
            CREATE TRIGGER common_http_fail_parse_update BEFORE UPDATE ON kv_store WHEN NEW.key='rust_parse_settings' BEGIN SELECT RAISE(ABORT, 'isolated settings failure'); END;")
            .map_err(|_| StorageError::Database)
    }).await.expect("install isolated write failure");
    let failed = private_json(
        server
            .admin_post(
                &cookie,
                &[("action", "set_parse_settings"), ("concurrency", "3")],
            )
            .header(header::ORIGIN, &server.origin),
        StatusCode::SERVICE_UNAVAILABLE,
    )
    .await;
    assert_eq!(failed["code"], 503);
    assert_eq!(failed["success"], false);
    assert_parse_settings(&parse_settings(&server, &cookie).await, 7);
    assert_eq!(
        server.storage.get("rust_parse_settings").await.unwrap(),
        Some(json!({"concurrency":7}))
    );
    assert!(
        before_kv == unrelated_parse_settings_kv(&server).await,
        "failed write changed unrelated KV"
    );
    server.storage.with_connection(|db| {
        db.execute_batch("DROP TRIGGER common_http_fail_parse_insert; DROP TRIGGER common_http_fail_parse_update;")
            .map_err(|_| StorageError::Database)
    }).await.expect("remove isolated write failure");
    let recovered = private_json(
        server
            .admin_post(
                &cookie,
                &[("action", "set_parse_settings"), ("concurrency", "3")],
            )
            .header(header::ORIGIN, &server.origin),
        StatusCode::OK,
    )
    .await;
    assert_parse_settings(&recovered, 3);
    assert_parse_settings(&parse_settings(&server, &cookie).await, 3);
    server.close().await;
    let server = Server::start(&db).await;
    assert_parse_settings(&parse_settings(&server, &cookie).await, 3);
    server.close().await;
}

#[tokio::test(flavor = "current_thread")]
async fn retention_defaults_validation_persistence_and_parse_only_cleanup() {
    let db = TestDb::new();
    let server = Server::start(&db).await;
    let cookie = server.login("/admin").await;
    let initial = private_json(
        server.admin_get(&cookie, &[("action", "get_log_settings")]),
        StatusCode::OK,
    )
    .await;
    assert_eq!(initial["data"]["log_retention_days"], 7);
    for bad in ["", "0", "-1", "3651", "1.5", "seven", "65536"] {
        let value = private_json(
            server.admin_post(
                &cookie,
                &[("action", "set_log_settings"), ("log_retention_days", bad)],
            ),
            StatusCode::BAD_REQUEST,
        )
        .await;
        assert_eq!(value["code"], 400);
    }
    server
        .storage
        .put("common_http_unrelated", json!({"keep":true}), None)
        .await
        .expect("seed unrelated KV");
    private_json(
        server.admin_post(
            &cookie,
            &[
                ("action", "cloud189_manual_save"),
                ("access_token", CLOUD_TOKEN),
            ],
        ),
        StatusCode::OK,
    )
    .await;
    let target = server.target(&[("url", SHARE), ("type", "json")]);
    let code = short_code(&server, &target).await;
    server.parse("https://unmigrated.invalid/old").await;
    server.parse("https://unmigrated.invalid/fresh").await;
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("wall clock")
        .as_millis() as i64;
    server.storage.with_connection(move |db| {
        db.execute("UPDATE parse_records SET created_ms=?1 WHERE id=(SELECT MIN(id) FROM parse_records)", [now_ms - 3 * 86_400_000]).map_err(|_| StorageError::Database)?;
        Ok(())
    }).await.expect("age only the old parse fixture");
    let before = private_json(
        server.admin_get(&cookie, &[("action", "count_records")]),
        StatusCode::OK,
    )
    .await;
    assert_eq!(before["total"], 2);
    let changed = private_json(
        server.admin_post(
            &cookie,
            &[("action", "set_log_settings"), ("log_retention_days", "1")],
        ),
        StatusCode::OK,
    )
    .await;
    assert_eq!(changed["data"]["log_retention_days"], 1);
    let remaining = private_json(
        server.admin_get(&cookie, &[("action", "get_records")]),
        StatusCode::OK,
    )
    .await;
    assert_eq!(remaining["data"]["total"], 1);
    assert_eq!(
        remaining["data"]["failed"][0]["url"],
        "https://unmigrated.invalid/fresh"
    );
    let cleared = private_json(
        server.admin_post(&cookie, &[("action", "clear_all_records")]),
        StatusCode::OK,
    )
    .await;
    assert_eq!(cleared["deleted_count"], 1);
    assert_eq!(
        server
            .storage
            .get("common_http_unrelated")
            .await
            .expect("read unrelated KV"),
        Some(json!({"keep":true}))
    );
    assert_eq!(status(&server).await["cloud189"]["logged_in"], true);
    server.close().await;

    let server = Server::start(&db).await;
    let settings = private_json(
        server.admin_get(&cookie, &[("action", "get_log_settings")]),
        StatusCode::OK,
    )
    .await;
    assert_eq!(settings["data"]["log_retention_days"], 1);
    let count = private_json(
        server.admin_get(&cookie, &[("action", "count_records")]),
        StatusCode::OK,
    )
    .await;
    assert_eq!(count["total"], 0);
    let stats = private_json(
        server.admin_get(&cookie, &[("action", "get_stats")]),
        StatusCode::OK,
    )
    .await;
    assert_eq!(stats["data"]["total"], 2);
    assert_eq!(stats["data"]["failed"], 2);
    assert_eq!(status(&server).await["cloud189"]["configured"], true);
    let value = json_response(server.get(&format!("/s/{code}")), StatusCode::OK).await;
    assert_eq!(value["code"], 503);
    server.close().await;
}

#[tokio::test(flavor = "current_thread")]
async fn history_crud_search_pagination_and_lifetime_statistics() {
    let db = TestDb::new();
    let server = Server::start(&db).await;
    let cookie = server.login("/admin").await;
    for name in ["alpha", "beta", "gamma"] {
        server
            .parse(&format!("https://unmigrated.invalid/{name}"))
            .await;
    }
    let listed = private_json(
        server.admin_get(
            &cookie,
            &[("action", "get_records"), ("page", "1"), ("limit", "2")],
        ),
        StatusCode::OK,
    )
    .await;
    assert_eq!(listed["data"]["total"], 3);
    assert_eq!(listed["data"]["successTotal"], 0);
    assert_eq!(listed["data"]["failedTotal"], 3);
    assert_eq!(listed["data"]["pages"], 2);
    let rows = listed["data"]["failed"]
        .as_array()
        .expect("failed history page");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["url"], "https://unmigrated.invalid/gamma");
    let newest_id = rows[0]["id"].as_i64().expect("record ID");
    assert!(newest_id > rows[1]["id"].as_i64().expect("record ID"));
    assert_eq!(rows[0]["code"], 501);
    assert_eq!(rows[0]["cached"], false);
    let searched = private_json(
        server.admin_get(&cookie, &[("action", "search_records"), ("search", "BETA")]),
        StatusCode::OK,
    )
    .await;
    assert_eq!(searched["data"]["total"], 1);
    assert_eq!(
        searched["data"]["failed"][0]["url"],
        "https://unmigrated.invalid/beta"
    );
    let failed = private_json(
        server.admin_get(
            &cookie,
            &[
                ("action", "get_failed_records"),
                ("page", "2"),
                ("limit", "2"),
            ],
        ),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        failed["data"]["records"]
            .as_array()
            .expect("failed records")
            .len(),
        1
    );
    assert_eq!(failed["data"]["has_prev"], true);
    assert_eq!(failed["data"]["has_next"], false);
    let successful = private_json(
        server.admin_get(&cookie, &[("action", "get_success_records")]),
        StatusCode::OK,
    )
    .await;
    assert_eq!(successful["data"]["total"], 0);
    assert!(
        successful["data"]["records"]
            .as_array()
            .expect("success records")
            .is_empty()
    );
    for fields in [
        vec![("action", "search_records"), ("search", " ")],
        vec![("action", "get_records"), ("type", "unknown")],
    ] {
        private_json(server.admin_get(&cookie, &fields), StatusCode::BAD_REQUEST).await;
    }
    for id in ["", "0", "-1", "not-an-id"] {
        private_json(
            server.admin_post(&cookie, &[("action", "delete_record"), ("record_id", id)]),
            StatusCode::BAD_REQUEST,
        )
        .await;
    }
    let id = newest_id.to_string();
    let deleted = private_json(
        server.admin_post(&cookie, &[("action", "delete_record"), ("record_id", &id)]),
        StatusCode::OK,
    )
    .await;
    assert_eq!(deleted["data"]["deleted"], true);
    let again = private_json(
        server.admin_post(&cookie, &[("action", "delete_record"), ("record_id", &id)]),
        StatusCode::OK,
    )
    .await;
    assert_eq!(again["data"]["deleted"], false);
    let count = private_json(
        server.admin_get(&cookie, &[("action", "count_records")]),
        StatusCode::OK,
    )
    .await;
    assert_eq!(count["total"], 2);
    assert_eq!(count["total"], count["data"]["total"]);
    let cleared = private_json(
        server.admin_post(&cookie, &[("action", "clear_all_records")]),
        StatusCode::OK,
    )
    .await;
    assert_eq!(cleared["deleted_count"], 2);
    assert_eq!(cleared["deleted_count"], cleared["data"]["deleted_count"]);
    let empty = private_json(
        server.admin_get(&cookie, &[("action", "count_records")]),
        StatusCode::OK,
    )
    .await;
    assert_eq!(empty["total"], 0);
    let stats = private_json(
        server.admin_get(&cookie, &[("action", "get_stats")]),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        stats["data"],
        json!({"total":3,"success":0,"failed":3,"cached":0})
    );
    server.parse("https://unmigrated.invalid/after-clear").await;
    let recreated = private_json(
        server.admin_get(&cookie, &[("action", "get_records")]),
        StatusCode::OK,
    )
    .await;
    assert!(
        recreated["data"]["failed"][0]["id"]
            .as_i64()
            .expect("new ID")
            > newest_id
    );
    server.close().await;
}

#[tokio::test(flavor = "current_thread")]
async fn batch_clear_preserves_top_level_loop_fields_and_lifetime_counters() {
    let db = TestDb::new();
    let server = Server::start(&db).await;
    let cookie = server.login("/admin").await;
    for index in 0..12 {
        server
            .parse(&format!("https://unmigrated.invalid/batch/{index}"))
            .await;
    }
    let cancelled = private_json(
        server.admin_post(&cookie, &[("action", "cancel_clear")]),
        StatusCode::OK,
    )
    .await;
    assert_eq!(cancelled["data"]["cancelled"], true);
    let count = private_json(
        server.admin_get(&cookie, &[("action", "count_records")]),
        StatusCode::OK,
    )
    .await;
    assert_eq!(count["total"], 12);
    for (deleted, remaining, finished) in [(10, 2, false), (2, 0, true), (0, 0, true)] {
        let batch = private_json(
            server.admin_post(
                &cookie,
                &[("action", "delete_batch_records"), ("limit", "100")],
            ),
            StatusCode::OK,
        )
        .await;
        assert_eq!(batch["deleted_count"], deleted);
        assert_eq!(batch["remaining"], remaining);
        assert_eq!(batch["finished"], finished);
        for key in ["deleted_count", "remaining", "finished"] {
            assert_eq!(batch[key], batch["data"][key]);
        }
    }
    let stats = private_json(
        server.admin_get(&cookie, &[("action", "get_stats")]),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        stats["data"],
        json!({"total":12,"success":0,"failed":12,"cached":0})
    );
    server.close().await;
}

#[tokio::test(flavor = "current_thread")]
async fn short_creation_enforces_origin_and_rejects_dangerous_targets() {
    let db = TestDb::new();
    let server = Server::start(&db).await;
    let target = server.target(&[("url", SHARE), ("type", "json")]);
    for origin in ["https://foreign.invalid", "null", "https://127.0.0.1"] {
        json_response(
            server
                .post("/api/create-short")
                .header(header::ORIGIN, origin)
                .json(&json!({"url":target})),
            StatusCode::FORBIDDEN,
        )
        .await;
    }
    json_response(
        server
            .post("/api/create-short")
            .header("sec-fetch-site", "cross-site")
            .json(&json!({"url":target})),
        StatusCode::FORBIDDEN,
    )
    .await;
    send(
        server.get("/api/create-short"),
        StatusCode::METHOD_NOT_ALLOWED,
    )
    .await;
    json_response(
        server.post("/api/create-short").body("{"),
        StatusCode::BAD_REQUEST,
    )
    .await;
    json_response(
        server.post("/api/create-short").json(&json!({})),
        StatusCode::BAD_REQUEST,
    )
    .await;
    json_response(
        server
            .post("/api/create-short")
            .body("x".repeat(16 * 1024 + 1)),
        StatusCode::PAYLOAD_TOO_LARGE,
    )
    .await;
    for bad in [
        "https://foreign.invalid/?url=share".to_owned(),
        "javascript:alert(1)".to_owned(),
        format!("{}/admin?url=share", server.origin),
        server.origin.clone(),
        target.replacen("http://", "http://user:synthetic@", 1),
    ] {
        json_response(
            server.post("/api/create-short").json(&json!({"url":bad})),
            StatusCode::BAD_REQUEST,
        )
        .await;
    }
    for key in [
        "action",
        "username",
        "password",
        "authorization",
        "auth",
        "access_token",
        "p189_token",
        "c189_token",
        "login_info",
        "cookie",
    ] {
        let dangerous = server.target(&[("url", SHARE), (key, "synthetic-private-value")]);
        let value = json_response(
            server
                .post("/api/create-short")
                .json(&json!({"url":dangerous})),
            StatusCode::BAD_REQUEST,
        )
        .await;
        assert_eq!(value["success"], false);
    }
    short_code(&server, &target).await;
    json_response(server.get("/s/missing"), StatusCode::NOT_FOUND).await;
    server.close().await;
}

#[tokio::test(flavor = "current_thread")]
async fn short_resolution_never_overrides_stored_target_from_query_or_body() {
    let db = TestDb::new();
    let server = Server::start(&db).await;
    let cookie = server.login("/admin").await;
    let target = server.target(&[
        ("url", SHARE),
        ("pwd", "stored-share-password"),
        ("type", "json"),
    ]);
    let code = short_code(&server, &target).await;
    let injected = [
        ("url", "https://unmigrated.invalid/injected"),
        ("pwd", "changed"),
        ("type", "down"),
        ("action", "cloud189_logout"),
        ("auth", "injected-auth"),
        ("id", "injected-id"),
    ];
    let path = format!("/s/{code}/ignored/suffix");
    let get = json_response(server.get(&path).query(&injected), StatusCode::OK).await;
    let form = json_response(
        server
            .post(&format!("/s/{code}"))
            .query(&injected)
            .form(&injected),
        StatusCode::OK,
    )
    .await;
    let json_body = json_response(server.post(&path).json(&json!({"url":"https://unmigrated.invalid/json-override","action":"cloud189_manual_save","access_token":CLOUD_TOKEN})), StatusCode::OK).await;
    for value in [get, form, json_body] {
        assert_eq!(value["code"], 503);
    }
    let records = private_json(
        server.admin_get(&cookie, &[("action", "get_records")]),
        StatusCode::OK,
    )
    .await;
    assert_eq!(records["data"]["total"], 3);
    for row in records["data"]["failed"]
        .as_array()
        .expect("short parse records")
    {
        assert_eq!(row["url"], SHARE);
        assert_eq!(row["pwd"], "stored-share-password");
    }
    assert_eq!(status(&server).await["cloud189"]["configured"], false);
    server.close().await;
}

#[tokio::test(flavor = "current_thread")]
async fn options_head_and_request_body_limits() {
    let db = TestDb::new();
    let server = Server::start(&db).await;
    for path in [
        "/",
        "/admin",
        "/admin/",
        "/api/create-short",
        "/s/missing",
        "/s/missing/suffix",
    ] {
        let response = send(
            server
                .client
                .request(reqwest::Method::OPTIONS, format!("{}{path}", server.origin)),
            StatusCode::NO_CONTENT,
        )
        .await;
        assert_eq!(response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
        assert_eq!(
            response.headers()[header::ACCESS_CONTROL_ALLOW_METHODS],
            "GET, POST, HEAD, OPTIONS"
        );
        assert_eq!(response.headers()[header::ALLOW], "GET, POST, HEAD");
        assert!(response.bytes().await.expect("OPTIONS body").is_empty());
    }
    for (path, expected) in [
        ("/", StatusCode::OK),
        ("/admin", StatusCode::OK),
        ("/admin/", StatusCode::OK),
        ("/s/missing", StatusCode::NOT_FOUND),
        ("/api/create-short", StatusCode::METHOD_NOT_ALLOWED),
    ] {
        let response = send(
            server.client.head(format!("{}{path}", server.origin)),
            expected,
        )
        .await;
        if path.starts_with("/admin") {
            assert_login(&response);
        }
        assert!(response.bytes().await.expect("HEAD body").is_empty());
    }
    let cookie = server.login("/admin").await;
    let response = send(
        server
            .client
            .head(format!("{}/admin?action=get_stats", server.origin))
            .header(header::COOKIE, &cookie),
        StatusCode::OK,
    )
    .await;
    assert_private(&response);
    assert!(
        response
            .bytes()
            .await
            .expect("private HEAD body")
            .is_empty()
    );
    let exact_limit = format!("padding={}", "x".repeat(64 * 1024 - 8));
    send(
        server
            .post("/")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(exact_limit.clone()),
        StatusCode::OK,
    )
    .await;
    for path in ["/", "/admin"] {
        let request = server
            .post(path)
            .header(header::COOKIE, &cookie)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(format!("{exact_limit}x"));
        let value = if path == "/admin" {
            private_json(request, StatusCode::PAYLOAD_TOO_LARGE).await
        } else {
            json_response(request, StatusCode::PAYLOAD_TOO_LARGE).await
        };
        assert_eq!(value["code"], 413);
    }
    json_response(
        server.post("/").json(&json!({"url":SHARE})),
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
    )
    .await;
    json_response(
        server
            .post("/")
            .header(header::CONTENT_TYPE, "multipart/form-data")
            .body("invalid multipart"),
        StatusCode::BAD_REQUEST,
    )
    .await;
    let oversized = "x".repeat(64 * 1024);
    json_response(
        server.multipart("/", &[("padding", &oversized)]),
        StatusCode::BAD_REQUEST,
    )
    .await;
    let file = format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"upload\"; filename=\"test.txt\"\r\nContent-Type: text/plain\r\n\r\nsynthetic\r\n--{BOUNDARY}--\r\n"
    );
    json_response(
        server
            .post("/")
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={BOUNDARY}"),
            )
            .body(file),
        StatusCode::BAD_REQUEST,
    )
    .await;
    let count = private_json(
        server.admin_get(&cookie, &[("action", "count_records")]),
        StatusCode::OK,
    )
    .await;
    assert_eq!(count["total"], 0);
    server.close().await;
}

#[tokio::test(flavor = "current_thread")]
async fn ordinary_multipart_and_form_parameter_precedence() {
    let db = TestDb::new();
    let server = Server::start(&db).await;
    let cookie = server.login("/admin").await;
    let first = "https://unmigrated.invalid/query-first";
    let ignored = "https://unmigrated.invalid/query-ignored";
    json_response(
        server
            .get("/")
            .query(&[("url", first), ("url", ignored), ("pwd", "query-password")]),
        StatusCode::OK,
    )
    .await;
    let last = "https://unmigrated.invalid/body-last";
    json_response(
        server
            .post("/")
            .query(&[("url", ignored), ("pwd", "query-password")])
            .form(&[
                ("url", "https://unmigrated.invalid/body-first"),
                ("url", last),
                ("pwd", "earlier-password"),
                ("pwd", ""),
            ]),
        StatusCode::OK,
    )
    .await;
    let multipart_url = "https://unmigrated.invalid/multipart-query";
    json_response(
        server
            .multipart(
                "/",
                &[
                    ("url", ignored),
                    ("url", ""),
                    ("pwd", "earlier-password"),
                    ("pwd", ""),
                    ("type", "json"),
                ],
            )
            .query(&[("url", multipart_url), ("pwd", "multipart-password")]),
        StatusCode::OK,
    )
    .await;
    let settings = private_json(
        server
            .multipart(
                "/admin/",
                &[("action", "set_log_settings"), ("log_retention_days", "9")],
            )
            .header(header::COOKIE, &cookie),
        StatusCode::OK,
    )
    .await;
    assert_eq!(settings["data"]["log_retention_days"], 9);
    let records = private_json(
        server.admin_get(&cookie, &[("action", "get_records")]),
        StatusCode::OK,
    )
    .await;
    let rows = records["data"]["failed"]
        .as_array()
        .expect("form parse records");
    assert_eq!(rows.len(), 3);
    for (row, url, pwd) in [
        (&rows[0], multipart_url, "multipart-password"),
        (&rows[1], last, "query-password"),
        (&rows[2], first, "query-password"),
    ] {
        assert_eq!(row["url"], url);
        assert_eq!(row["pwd"], pwd);
        assert_eq!(row["code"], 501);
    }
    server.close().await;
}

#[tokio::test(flavor = "current_thread")]
async fn public_home_stats_are_aggregate_only_and_match_private_lifetime_counts() {
    let db = TestDb::new();
    let server = Server::start(&db).await;
    server.parse("https://unmigrated.invalid/synthetic").await;
    let public = json_response(server.get("/?action=get_stats"), StatusCode::OK).await;
    assert_eq!(
        public["data"],
        json!({"total":1,"success":0,"failed":1,"cached":0})
    );
    assert_status_redacted(&public);
    let cookie = server.login("/admin").await;
    let private = private_json(
        server.admin_get(&cookie, &[("action", "get_stats")]),
        StatusCode::OK,
    )
    .await;
    assert_eq!(public, private);
    server.close().await;
}
