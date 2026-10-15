//! Short-link creation and resolution into the normal parse path.
use super::{
    AppState,
    home::parsed,
    params::Params,
    reply::Reply,
    security::{same_origin, trusted_outer},
};
use axum::{
    body::to_bytes,
    extract::{Request, State},
    http::StatusCode,
    response::Response,
};
use serde_json::{Value, json};
use std::time::Duration;

pub(super) async fn create_short(State(state): State<AppState>, request: Request) -> Response {
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
pub(super) async fn short(State(state): State<AppState>, request: Request) -> Response {
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
