//! Cookie access and trusted same-origin policy shared by handlers.
use super::AppState;
use axum::http::{HeaderMap, Method, header};

pub(super) fn cookie(headers: &HeaderMap) -> &str {
    headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
}
pub(super) fn safe_post(state: &AppState, method: &Method, headers: &HeaderMap) -> bool {
    *method == Method::POST && same_origin(state, headers)
}
pub(super) fn same_origin(state: &AppState, headers: &HeaderMap) -> bool {
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
pub(super) fn trusted_outer(state: &AppState, headers: &HeaderMap, url: &url::Url) -> bool {
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
pub(super) fn secure(state: &AppState) -> &'static str {
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
