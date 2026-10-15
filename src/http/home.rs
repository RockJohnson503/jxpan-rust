//! Homepage actions and cache-backed parse dispatch.
use super::{
    AppState, HOME,
    actions::records_action,
    params::Params,
    parse,
    presentation::{action_response, page, private, unsupported},
    reply::Reply,
    request::parameters,
    security::{cookie, safe_post},
};
use crate::cache::CacheSource;
use axum::{
    extract::{Request, State},
    http::StatusCode,
    response::Response,
};

pub(super) async fn home(State(state): State<AppState>, request: Request) -> Response {
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
pub(super) async fn parsed(state: AppState, params: Params) -> Response {
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
