use axum::http::{StatusCode, header};

use crate::providers::{
    cloud189,
    cloud189_client::Cloud189Client,
    cloud189_service, mcloud,
    mcloud_client::MobileCloudClient,
    mcloud_service::{self, ServiceConfig},
};

use super::{AppState, params::Params, reply::Reply};

pub async fn run(state: AppState, params: Params) -> Reply {
    if cloud189::extract_share_info(params.get("url")).is_some() {
        return cloud189(state, params).await;
    }
    if mcloud::extract_share_info(params.get("url")).is_some() {
        return mobile(state, params).await;
    }
    Reply::error(StatusCode::NOT_IMPLEMENTED, "该网盘尚未迁移")
}

async fn mobile(state: AppState, params: Params) -> Reply {
    let authorization = state.credentials["mcloud"]["authorization"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or(params.get("auth"))
        .to_owned();
    let configured = !authorization.is_empty();
    let client = MobileCloudClient::new(state.http, authorization);
    if params.get("type") == "down" && !params.file_id().is_empty() {
        if !configured {
            return Reply::error(StatusCode::BAD_GATEWAY, "移动云盘凭据未配置");
        }
        let sid = if params.get("sid").is_empty() {
            mcloud::extract_share_info(params.get("url")).unwrap_or_default()
        } else {
            params.get("sid").to_owned()
        };
        return match client.download_url(&sid, params.file_id()).await {
            Ok(Some(url)) => Reply::redirect(url),
            Ok(None) => Reply::error(StatusCode::BAD_GATEWAY, "移动云盘上游拒绝下载请求"),
            Err(reason) => Reply::error(
                StatusCode::BAD_GATEWAY,
                &format!("移动云盘获取下载链接失败: {reason}"),
            ),
        };
    }
    Reply::json(
        mcloud_service::parse(
            &client,
            ServiceConfig {
                enabled: true,
                authorization_configured: configured,
            },
            params.get("url"),
            params.get("pwd"),
            &params.folders(),
            params.file_id(),
        )
        .await,
    )
}

async fn cloud189(state: AppState, params: Params) -> Reply {
    let mut credentials = state.credentials["cloud189"].clone();
    if credentials["open_access_token"]
        .as_str()
        .is_none_or(str::is_empty)
        && !params.get("p189_token").is_empty()
    {
        credentials["open_access_token"] = params.get("p189_token").into();
    }
    let configured = credentials["open_access_token"]
        .as_str()
        .is_some_and(|s| !s.is_empty())
        || (credentials["session_key"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
            && credentials["session_secret"]
                .as_str()
                .is_some_and(|s| !s.is_empty()));
    if !configured {
        return Reply::json(serde_json::json!({
            "code":401,"msg":"天翼云盘凭据未配置","success":false,"data":null
        }));
    }
    let client = Cloud189Client::new(state.http.clone(), credentials);
    if params.get("type") == "down" && !params.file_id().is_empty() {
        let url = match cloud189_service::fast_download(
            client,
            params.get("url"),
            params.get("pwd"),
            params.file_id(),
            params.get("sid"),
        )
        .await
        {
            Ok(url) => url,
            Err(value) => return Reply::fast_error(value),
        };
        // Match the deployed fast path's one-byte Range hop. Drop the body immediately.
        let url = resolve_range_hop(&state.http, url).await;
        return Reply::redirect(url);
    }
    Reply::json(
        cloud189_service::parse(
            client,
            params.get("url"),
            params.get("pwd"),
            &params.folders(),
            params.file_id(),
        )
        .await,
    )
}

async fn resolve_range_hop(client: &reqwest::Client, url: String) -> String {
    let Ok(parsed) = url::Url::parse(&url) else {
        return url;
    };
    if parsed.scheme() != "https"
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return url;
    }
    let Ok(response) = client
        .get(parsed)
        .header(header::RANGE, "bytes=0-0")
        .send()
        .await
    else {
        return url;
    };
    if response.status().is_redirection()
        && let Some(location) = response
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
    {
        return location.to_owned();
    }
    url
}
