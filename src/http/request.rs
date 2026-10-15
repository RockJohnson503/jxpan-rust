//! Bounded query and form decoding for HTTP handlers.
use super::{AppState, FORM_LIMIT, params::Params, reply::Reply};
use axum::{
    body::to_bytes,
    extract::{FromRequest, Multipart, Request},
    http::{Method, StatusCode, header},
    response::Response,
};
use std::time::Duration;

pub(super) async fn parameters(request: Request, state: &AppState) -> Result<Params, Response> {
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
