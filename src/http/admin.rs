//! Admin authentication, authorization, and action dispatch.
use super::{
    ADMIN, ADMIN_LOGIN, AppState,
    actions::records_action,
    presentation::{action_response, login_error, login_response, page, private, set_cookie},
    reply::Reply,
    request::parameters,
    security::{cookie, safe_post, secure},
};
use axum::{
    extract::{Request, State},
    http::{Method, StatusCode},
    response::Response,
};

pub(super) async fn admin(State(state): State<AppState>, request: Request) -> Response {
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
