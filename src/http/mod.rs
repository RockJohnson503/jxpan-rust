pub mod params;
mod parse;
mod reply;
mod state;

pub use state::AppState;

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::get,
};
use params::Params;
use reply::{Reply, cors};

const HOME: &[u8] = include_bytes!("../../web/index.html");
const ADMIN_LOGIN: &[u8] = include_bytes!("../../web/admin-login.html");

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(home).post(home).options(options))
        .route("/admin", get(admin_login))
        .with_state(state)
}

async fn home(State(state): State<AppState>, request: Request) -> Response {
    if request.uri().to_string().len() > 16 * 1024 {
        return Reply::error(StatusCode::URI_TOO_LONG, "请求地址过长").response();
    }
    let mut params = Params::query(request.uri().query());
    if request.method() == Method::POST {
        let content_type = request
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !content_type.starts_with("application/x-www-form-urlencoded") {
            return unsupported();
        }
        let Ok(body) = to_bytes(request.into_body(), 64 * 1024).await else {
            return Reply::error(StatusCode::PAYLOAD_TOO_LARGE, "请求正文过大").response();
        };
        params.apply_form(&body);
    }
    if params.get("url").is_empty() && params.get("action").is_empty() {
        return page(HOME);
    }
    if !params.get("action").is_empty() {
        return unsupported();
    }
    let key = state.cache_key(&params);
    let cache = if params.get("type") == "down" {
        state.download_cache.clone()
    } else {
        state.parse_cache.clone()
    };
    match cache
        .get_or_init(
            key,
            move || parse::run(state, params),
            Reply::retained_bytes,
        )
        .await
    {
        Ok(reply) => reply.response(),
        Err(_) => Reply::error(
            StatusCode::SERVICE_UNAVAILABLE,
            "并发解析已达上限，请稍后重试",
        )
        .response(),
    }
}

async fn admin_login(uri: Uri) -> Response {
    let params = Params::query(uri.query());
    if !params.get("action").is_empty() && params.get("action") != "login" {
        return unsupported();
    }
    page(ADMIN_LOGIN)
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
