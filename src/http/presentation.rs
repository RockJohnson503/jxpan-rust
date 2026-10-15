//! Static assets and shared response/header presentation.
use super::{
    ADMIN_LOGIN, QRCODE,
    reply::{Reply, cors},
};
use axum::{
    body::Body,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::Value;

pub(super) fn set_cookie(mut response: Response, value: String) -> Response {
    response.headers_mut().insert(
        header::SET_COOKIE,
        header::HeaderValue::from_str(&value).expect("generated cookie"),
    );
    response
}
pub(super) fn login_response(response: Response) -> Response {
    let mut response = private(response);
    // Keep the original native form usable on HTTP LAN hosts without Fetch Metadata.
    // Only this static login document permits same-origin referrers, never cross-origin.
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        header::HeaderValue::from_static("same-origin"),
    );
    response
}
pub(super) fn login_error(message: &str) -> Response {
    let html = String::from_utf8_lossy(ADMIN_LOGIN).replace("请登录以访问管理功能", message);
    let mut response = Response::new(Body::from(html));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/html; charset=utf-8"),
    );
    login_response(response)
}
pub(super) fn action_response(value: Value) -> Response {
    let status = value["code"]
        .as_u64()
        .and_then(|v| u16::try_from(v).ok())
        .and_then(|v| StatusCode::from_u16(v).ok())
        .unwrap_or(StatusCode::OK);
    let mut response = Reply::json(value).response();
    *response.status_mut() = status;
    response
}
pub(super) fn private(mut response: Response) -> Response {
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
pub(super) async fn qrcode_asset() -> Response {
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
pub(super) async fn options() -> Response {
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
pub(super) fn unsupported() -> Response {
    Reply::error(StatusCode::NOT_IMPLEMENTED, "该功能尚未迁移").response()
}
pub(super) fn page(content: &'static [u8]) -> Response {
    let mut response = Response::new(Body::from(content));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/html; charset=utf-8"),
    );
    cors(response)
}
