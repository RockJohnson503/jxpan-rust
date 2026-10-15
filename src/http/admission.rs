//! Request admission, deadline, and response-body permit lifetime.
use super::{AppState, reply::Reply};
use axum::{
    body::{Body, HttpBody},
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::Next,
    response::Response,
};
use futures_util::StreamExt;
use std::time::Duration;

pub(super) async fn admission(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
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
