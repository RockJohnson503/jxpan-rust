pub mod params;
mod parse;
mod reply;
mod state;
use crate::cache::CacheSource;
use axum::{
    Router,
    body::{Body, HttpBody, to_bytes},
    extract::{FromRequest, Multipart, Request, State},
    http::{HeaderMap, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::StreamExt;
use params::Params;
use reply::{Reply, cors};
use serde_json::{Value, json};
pub use state::AppState;
use std::time::Duration;

const HOME: &[u8] = include_bytes!("../../web/index.html");
const ADMIN_LOGIN: &[u8] = include_bytes!("../../web/admin-login.html");
const ADMIN: &[u8] = include_bytes!("../../web/admin.html");
const QRCODE: &[u8] = include_bytes!("../../web/vendor/qrcode.js");
const FORM_LIMIT: usize = 64 * 1024;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(home).post(home).options(options))
        .route("/admin", get(admin).post(admin).options(options))
        .route("/admin/", get(admin).post(admin).options(options))
        .route("/assets/qrcode.js", get(qrcode_asset))
        .route("/api/create-short", post(create_short).options(options))
        .route("/s/{code}", get(short).post(short).options(options))
        .route("/s/{code}/{*rest}", get(short).post(short).options(options))
        .layer(axum::extract::DefaultBodyLimit::max(FORM_LIMIT))
        .layer(middleware::from_fn_with_state(state.clone(), admission))
        .with_state(state)
}
async fn admission(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let Ok(permit) = state.requests.clone().try_acquire_owned() else {
        return Reply::error(
            StatusCode::SERVICE_UNAVAILABLE,
            "请求并发已达上限，请稍后重试",
        )
        .response();
    };
    let mut response = match tokio::time::timeout(Duration::from_secs(150), next.run(request)).await
    {
        Ok(response) => response,
        Err(_) => Reply::error(StatusCode::GATEWAY_TIMEOUT, "请求处理超时，请重试").response(),
    };
    // Stream wrapping otherwise loses Body's exact size hint and turns fixed
    // JSON/static replies into chunked HTTP. Preserve the original framing.
    if !response.headers().contains_key(header::CONTENT_LENGTH)
        && let Some(length) = response.body().size_hint().exact()
    {
        response.headers_mut().insert(
            header::CONTENT_LENGTH,
            header::HeaderValue::from_str(&length.to_string()).expect("body length"),
        );
    }
    // Keep admission until the (small JSON/302/static) response is sent or dropped.
    response.map(|body| {
        Body::from_stream(body.into_data_stream().map(move |chunk| {
            let _keep_admission = &permit;
            chunk
        }))
    })
}
async fn parameters(request: Request, state: &AppState) -> Result<Params, Response> {
    tokio::time::timeout(Duration::from_secs(15), parameters_inner(request, state))
        .await
        .unwrap_or_else(|_| {
            Err(Reply::error(StatusCode::REQUEST_TIMEOUT, "请求正文读取超时").response())
        })
}
async fn parameters_inner(request: Request, state: &AppState) -> Result<Params, Response> {
    if request.uri().to_string().len() > 16 * 1024 {
        return Err(Reply::error(StatusCode::URI_TOO_LONG, "请求地址过长").response());
    }
    let mut params = Params::query(request.uri().query());
    if request.method() != Method::POST {
        return Ok(params);
    }
    let content_type = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    if content_type.starts_with("application/x-www-form-urlencoded") {
        let body = to_bytes(request.into_body(), FORM_LIMIT)
            .await
            .map_err(|_| Reply::error(StatusCode::PAYLOAD_TOO_LARGE, "请求正文过大").response())?;
        params.apply_form(&body);
    } else if content_type.starts_with("multipart/form-data") {
        let mut form = Multipart::from_request(request, state)
            .await
            .map_err(|_| Reply::error(StatusCode::BAD_REQUEST, "表单格式错误").response())?;
        let mut fields = 0;
        while let Some(field) = form
            .next_field()
            .await
            .map_err(|_| Reply::error(StatusCode::BAD_REQUEST, "表单格式错误或过大").response())?
        {
            fields += 1;
            if fields > 128 || field.file_name().is_some() {
                return Err(Reply::error(StatusCode::BAD_REQUEST, "仅支持普通文本表单").response());
            }
            let name = field.name().unwrap_or("").to_owned();
            let value = field.text().await.map_err(|_| {
                Reply::error(StatusCode::BAD_REQUEST, "表单格式错误或过大").response()
            })?;
            params.apply_field(name, value);
        }
    } else {
        return Err(Reply::error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "仅支持 URL 编码或 multipart 表单",
        )
        .response());
    }
    Ok(params)
}
async fn home(State(state): State<AppState>, request: Request) -> Response {
    let method = request.method().clone();
    let headers = request.headers().clone();
    let params = match parameters(request, &state).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    if params.get("url").is_empty() && params.get("action").is_empty() {
        return page(HOME);
    }
    let action = params.get("action");
    if !action.is_empty() {
        // The original homepage displays aggregate counters; no private history is exposed.
        if action == "get_stats" {
            return records_action(&state, &params).await;
        }
        let public = matches!(action, "login_status" | "cloud189_status");
        let parse_action = matches!(
            action,
            "parse_mcloud_subfolder" | "parse_cloud189_file" | "parse_cloud189_folder_item"
        );
        if !parse_action {
            let authorized = if public {
                false
            } else {
                match state.admin.authorized(cookie(&headers)).await {
                    Ok(allowed) => allowed,
                    Err(_) => {
                        return private(
                            Reply::error(StatusCode::SERVICE_UNAVAILABLE, "后台存储不可用")
                                .response(),
                        );
                    }
                }
            };
            if !public && (!authorized || !safe_post(&state, &method, &headers)) {
                return private(
                    Reply::error(StatusCode::FORBIDDEN, "需要后台登录及同源 POST 请求").response(),
                );
            }
            if let Some(value) = state.auth.handle(action, &params, authorized).await {
                state.account();
                return private(action_response(value));
            }
            return private(unsupported());
        }
    }
    parsed(state, params).await
}
async fn parsed(state: AppState, params: Params) -> Response {
    let (revision, credentials) = state.account();
    let key = state.cache_key(&params, revision);
    let fast = params.get("type") == "down" && !params.file_id().is_empty();
    let cache = if fast {
        state.download_cache.clone()
    } else {
        state.parse_cache.clone()
    };
    let record_hit = !fast && params.get("action").is_empty();
    let loading_state = state.clone();
    let reply = cache
        .get_or_init_with_source(
            key,
            move || async move { parse::run(loading_state, params, revision, credentials).await },
            Reply::retained_bytes,
        )
        .await;
    match reply {
        Ok((reply, source)) => {
            let counted = match source {
                CacheSource::Loaded => Ok(()),
                CacheSource::Hit if record_hit => state.records.record_cache_hit().await,
                CacheSource::Joined if record_hit => {
                    state
                        .records
                        .record_request_outcome(reply.request_success, false)
                        .await
                }
                _ => Ok(()),
            };
            if counted.is_err() {
                return Reply::error(StatusCode::SERVICE_UNAVAILABLE, "统计保存失败，请稍后重试")
                    .response();
            }
            reply.response()
        }
        Err(_) => Reply::error(
            StatusCode::SERVICE_UNAVAILABLE,
            "并发解析已达上限，请稍后重试",
        )
        .response(),
    }
}
async fn admin(State(state): State<AppState>, request: Request) -> Response {
    let method = request.method().clone();
    let headers = request.headers().clone();
    let params = match parameters(request, &state).await {
        Ok(p) => p,
        Err(r) => return private(r),
    };
    let action = params.get("action");
    if action == "login" {
        if method != Method::POST {
            return login_response(page(ADMIN_LOGIN));
        }
        if !safe_post(&state, &method, &headers) {
            return private(Reply::error(StatusCode::FORBIDDEN, "登录必须是同源请求").response());
        }
        if !state.admin.configured() {
            return login_error("后台面板未配置，请设置后台账号和密码");
        }
        return match state
            .admin
            .login(params.get("username"), params.get("password"))
            .await
        {
            Ok(Some(token)) => set_cookie(
                private(page(ADMIN)),
                format!(
                    "admin_token={token}; Path=/; Max-Age=604800; HttpOnly; SameSite=Lax{}",
                    secure(&state)
                ),
            ),
            Ok(None) => login_error("用户名或密码错误，或尝试过于频繁"),
            Err(_) => private(
                Reply::error(StatusCode::SERVICE_UNAVAILABLE, "后台登录存储不可用").response(),
            ),
        };
    }
    let authorized = match state.admin.authorized(cookie(&headers)).await {
        Ok(v) => v,
        Err(_) => {
            return private(
                Reply::error(StatusCode::SERVICE_UNAVAILABLE, "后台存储不可用").response(),
            );
        }
    };
    if !authorized {
        return if action.is_empty() {
            login_response(page(ADMIN_LOGIN))
        } else {
            private(Reply::error(StatusCode::UNAUTHORIZED, "请先登录后台").response())
        };
    }
    let read = matches!(
        action,
        "" | "get_stats"
            | "get_records"
            | "search_records"
            | "get_success_records"
            | "get_failed_records"
            | "count_records"
            | "get_log_settings"
            | "get_parse_settings"
            | "login_status"
            | "cloud189_status"
    );
    if !read && !safe_post(&state, &method, &headers) {
        return private(
            Reply::error(StatusCode::FORBIDDEN, "后台修改必须使用同源 POST").response(),
        );
    }
    if action == "logout" {
        return match state.admin.logout(cookie(&headers)).await {
            Ok(()) => set_cookie(
                login_response(page(ADMIN_LOGIN)),
                format!(
                    "admin_token=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax{}",
                    secure(&state)
                ),
            ),
            Err(_) => {
                private(Reply::error(StatusCode::SERVICE_UNAVAILABLE, "退出登录失败").response())
            }
        };
    }
    if action.is_empty() {
        return private(page(ADMIN));
    }
    if let Some(value) = state.auth.handle(action, &params, true).await {
        state.account();
        return private(action_response(value));
    }
    private(records_action(&state, &params).await)
}
async fn records_action(state: &AppState, params: &Params) -> Response {
    use crate::storage::StorageError;
    let page_num = number(params.get("page"), 1);
    let limit = number(params.get("limit"), 10).min(50);
    let outcome: Result<Value, StorageError> = match params.get("action") {
        "get_stats" => state.records.stats().await,
        "get_log_settings" => state
            .records
            .retention_days()
            .await
            .map(|days| json!({"log_retention_days":days})),
        "get_parse_settings" => Ok(state.parse_settings.to_json()),
        "set_parse_settings" => match params.get("concurrency").parse::<usize>() {
            Ok(concurrency) if (1..=crate::settings::MAX_CONCURRENCY).contains(&concurrency) => {
                state
                    .parse_settings
                    .set_concurrency(concurrency)
                    .await
                    .map(|()| state.parse_settings.to_json())
            }
            _ => {
                return Reply::error(StatusCode::BAD_REQUEST, "上游并发数必须是 1–16 的整数")
                    .response();
            }
        },
        "set_log_settings" => match params.get("log_retention_days").parse::<u16>() {
            Ok(days) if (1..=3650).contains(&days) => state
                .records
                .set_retention_days(days)
                .await
                .map(|()| json!({"log_retention_days":days})),
            _ => {
                return Reply::error(StatusCode::BAD_REQUEST, "日志保留天数必须是 1–3650 的整数")
                    .response();
            }
        },
        "get_records" | "search_records" => {
            if params.get("action") == "search_records" && params.get("search").trim().is_empty() {
                return Reply::error(StatusCode::BAD_REQUEST, "缺少搜索参数").response();
            }
            state
                .records
                .list(page_num, limit, params.get("type"), params.get("search"))
                .await
        }
        "get_success_records" | "get_failed_records" => {
            let kind = if params.get("action") == "get_success_records" {
                "success"
            } else {
                "failed"
            };
            state.records.list(page_num,limit,kind,params.get("search")).await.map(|data|json!({"records":data[kind],"total":data["total"],"current_page":page_num,"per_page":limit,"total_pages":data["pages"],"has_prev":page_num>1,"has_next":u64::from(page_num)<data["pages"].as_u64().unwrap_or(1)}))
        }
        "delete_record" => match params.get("record_id").parse::<i64>() {
            Ok(id) if id > 0 => state
                .records
                .delete(id)
                .await
                .map(|deleted| json!({"deleted":deleted})),
            _ => return Reply::error(StatusCode::BAD_REQUEST, "缺少或无效的 record_id").response(),
        },
        "clear_all_records" => state
            .records
            .clear()
            .await
            .map(|count| json!({"deleted_count":count})),
        "delete_batch_records" => {
            match state.records.delete_batch(number(params.get("limit"),10).clamp(1,10)).await {
                Ok(count)=>state.records.count().await.map(|remaining|json!({"deleted_count":count,"remaining":remaining,"finished":remaining==0})),
                Err(e)=>Err(e),
            }
        }
        "count_records" => state
            .records
            .count()
            .await
            .map(|total| json!({"total":total})),
        "cancel_clear" => Ok(json!({"cancelled":true})),
        _ => return unsupported(),
    };
    match outcome {
        Ok(data) => {
            let mut value = json!({"code":200,"msg":"操作成功","success":true,"data":data});
            // Preserve the old clear-loop top-level fields used by the imported page.
            if matches!(
                params.get("action"),
                "clear_all_records" | "delete_batch_records" | "count_records"
            ) && let Some(object) = value["data"].as_object().cloned()
            {
                value.as_object_mut().unwrap().extend(object);
            }
            Reply::json(value).response()
        }
        Err(StorageError::InvalidJson) => Reply::error(
            StatusCode::BAD_REQUEST,
            "筛选参数无效或记录页过大，请缩小范围",
        )
        .response(),
        Err(_) => Reply::error(StatusCode::SERVICE_UNAVAILABLE, "记录存储操作失败").response(),
    }
}
async fn create_short(State(state): State<AppState>, request: Request) -> Response {
    let headers = request.headers().clone();
    if !same_origin(&state, &headers) {
        return Reply::error(StatusCode::FORBIDDEN, "短链创建必须同源").response();
    }
    let bytes = match tokio::time::timeout(
        Duration::from_secs(15),
        to_bytes(request.into_body(), 16 * 1024),
    )
    .await
    {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(_)) => {
            return Reply::error(StatusCode::PAYLOAD_TOO_LARGE, "短链请求过大").response();
        }
        Err(_) => return Reply::error(StatusCode::REQUEST_TIMEOUT, "请求正文读取超时").response(),
    };
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return Reply::error(StatusCode::BAD_REQUEST, "短链请求不是有效 JSON").response();
    };
    let Some(original) = value["url"].as_str() else {
        return Reply::error(StatusCode::BAD_REQUEST, "缺少 URL 参数").response();
    };
    let Ok(url) = url::Url::parse(original) else {
        return Reply::error(StatusCode::BAD_REQUEST, "短链地址无效").response();
    };
    if !trusted_outer(&state, &headers, &url) {
        return Reply::error(StatusCode::BAD_REQUEST, "只能缩短本站解析地址").response();
    }
    match state.shortlinks.create(original).await {
        Ok(code) => {
            Reply::json(json!({"success":true,"shortcode":code,"msg":"短链创建成功"})).response()
        }
        Err(crate::shortlinks::ShortLinkError::InvalidUrl) => {
            Reply::error(StatusCode::BAD_REQUEST, "短链参数无效或包含凭据/管理动作").response()
        }
        Err(_) => Reply::error(StatusCode::SERVICE_UNAVAILABLE, "短链保存失败").response(),
    }
}
async fn short(State(state): State<AppState>, request: Request) -> Response {
    let code = request.uri().path().split('/').nth(2).unwrap_or("");
    match state.shortlinks.resolve(code).await {
        Ok(Some(target)) => parsed(state, Params::query(target.split_once('?').map(|p| p.1))).await,
        Ok(None) | Err(crate::shortlinks::ShortLinkError::InvalidCode) => {
            Reply::error(StatusCode::NOT_FOUND, "短链不存在").response()
        }
        Err(crate::shortlinks::ShortLinkError::InvalidUrl) => {
            Reply::error(StatusCode::BAD_REQUEST, "短链数据包含不允许的参数").response()
        }
        Err(_) => Reply::error(StatusCode::SERVICE_UNAVAILABLE, "短链存储不可用").response(),
    }
}
fn cookie(headers: &HeaderMap) -> &str {
    headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
}
fn safe_post(state: &AppState, method: &Method, headers: &HeaderMap) -> bool {
    *method == Method::POST && same_origin(state, headers)
}
fn same_origin(state: &AppState, headers: &HeaderMap) -> bool {
    if headers
        .get("sec-fetch-site")
        .is_some_and(|v| v == "cross-site")
    {
        return false;
    }
    let Some(value) = headers.get(header::ORIGIN) else {
        return true;
    };
    // Native POST navigation from a no-referrer page serializes Origin as null.
    // Only browser Fetch Metadata proving exact same-origin may recover that case;
    // opaque, absent, same-site and cross-site metadata are not sufficient.
    if value == "null" {
        return headers
            .get("sec-fetch-site")
            .is_some_and(|site| site == "same-origin");
    }
    value
        .to_str()
        .ok()
        .and_then(|v| url::Url::parse(v).ok())
        .is_some_and(|url| trusted_outer(state, headers, &url))
}
fn trusted_outer(state: &AppState, headers: &HeaderMap, url: &url::Url) -> bool {
    if !url.username().is_empty()
        || url.password().is_some()
        || !matches!(url.scheme(), "http" | "https")
    {
        return false;
    }
    if let Some(origin) = &state.public_origin {
        return url.origin() == origin.origin();
    }
    headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .and_then(|host| url::Url::parse(&format!("http://{host}")).ok())
        .is_some_and(|host| url.scheme() == "http" && url.origin() == host.origin())
}
fn secure(state: &AppState) -> &'static str {
    if state
        .public_origin
        .as_ref()
        .is_some_and(|u| u.scheme() == "https")
    {
        "; Secure"
    } else {
        ""
    }
}
fn number(input: &str, default: u32) -> u32 {
    input
        .parse::<u32>()
        .ok()
        .filter(|v| *v > 0)
        .unwrap_or(default)
}
fn set_cookie(mut response: Response, value: String) -> Response {
    response.headers_mut().insert(
        header::SET_COOKIE,
        header::HeaderValue::from_str(&value).expect("generated cookie"),
    );
    response
}
fn login_response(response: Response) -> Response {
    let mut response = private(response);
    // Keep the original native form usable on HTTP LAN hosts without Fetch Metadata.
    // Only this static login document permits same-origin referrers, never cross-origin.
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        header::HeaderValue::from_static("same-origin"),
    );
    response
}
fn login_error(message: &str) -> Response {
    let html = String::from_utf8_lossy(ADMIN_LOGIN).replace("请登录以访问管理功能", message);
    let mut response = Response::new(Body::from(html));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/html; charset=utf-8"),
    );
    login_response(response)
}
fn action_response(value: Value) -> Response {
    let status = value["code"]
        .as_u64()
        .and_then(|v| u16::try_from(v).ok())
        .and_then(|v| StatusCode::from_u16(v).ok())
        .unwrap_or(StatusCode::OK);
    let mut response = Reply::json(value).response();
    *response.status_mut() = status;
    response
}
fn private(mut response: Response) -> Response {
    response
        .headers_mut()
        .remove(header::ACCESS_CONTROL_ALLOW_ORIGIN);
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        header::HeaderValue::from_static("no-referrer"),
    );
    response
        .headers_mut()
        .insert("x-frame-options", header::HeaderValue::from_static("DENY"));
    response
}
async fn qrcode_asset() -> Response {
    let mut response = Response::new(Body::from(QRCODE));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/javascript; charset=utf-8"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("public, max-age=3600"),
    );
    response.headers_mut().insert(
        "x-content-type-options",
        header::HeaderValue::from_static("nosniff"),
    );
    response
}
async fn options() -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    for (name, value) in [
        (
            header::ACCESS_CONTROL_ALLOW_METHODS,
            "GET, POST, HEAD, OPTIONS",
        ),
        (header::ACCESS_CONTROL_ALLOW_HEADERS, "*"),
        (header::ACCESS_CONTROL_MAX_AGE, "2592000"),
        (header::ALLOW, "GET, POST, HEAD"),
    ] {
        response
            .headers_mut()
            .insert(name, header::HeaderValue::from_static(value));
    }
    cors(response)
}
fn unsupported() -> Response {
    Reply::error(StatusCode::NOT_IMPLEMENTED, "该功能尚未迁移").response()
}
fn page(content: &'static [u8]) -> Response {
    let mut response = Response::new(Body::from(content));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/html; charset=utf-8"),
    );
    cors(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn coalesced_failed_parses_count_two_failures_without_duplicate_history() {
        use crate::storage::Storage;
        use futures_util::{future::join, poll};
        let storage = Storage::open(":memory:", 16).await.unwrap();
        for key in ["c189_login_default", "mcloud_login_default"] {
            storage
                .put(key, json!({"_jxpan_logged_out":true}), None)
                .await
                .unwrap();
        }
        let state = AppState::new(storage.clone(), json!({}), "", "")
            .await
            .unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let actor = storage.clone();
        let blocker = tokio::spawn(async move {
            actor
                .with_connection(move |_| {
                    started_tx.send(()).unwrap();
                    release_rx
                        .recv_timeout(std::time::Duration::from_secs(5))
                        .unwrap();
                    Ok(())
                })
                .await
                .unwrap();
        });
        started_rx.await.unwrap();
        let query = Some("url=https%3A%2F%2Funmigrated.invalid%2Fshare&type=json");
        let mut first = Box::pin(parsed(state.clone(), Params::query(query)));
        assert!(poll!(first.as_mut()).is_pending());
        let mut second = Box::pin(parsed(state.clone(), Params::query(query)));
        assert!(poll!(second.as_mut()).is_pending());
        release_tx.send(()).unwrap();
        let (first, second) = join(first, second).await;
        for response in [first, second] {
            assert_eq!(response.status(), StatusCode::OK);
            let value: Value =
                serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap())
                    .unwrap();
            assert_eq!(value["code"], 501);
            assert_eq!(value["success"], false);
        }
        blocker.await.unwrap();
        assert_eq!(
            state.records.stats().await.unwrap(),
            json!({"total":2,"success":0,"failed":2,"cached":0})
        );
        assert_eq!(state.records.count().await.unwrap(), 1);
        state.close().await.unwrap();
    }
}
