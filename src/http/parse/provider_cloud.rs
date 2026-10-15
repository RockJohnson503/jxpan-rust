//! Cloud189 provider request adaptation; parse lifecycle remains in the parent.
use super::{AppState, Params, ParseOutcome, Reply, directory};
use crate::providers::{cloud189_client::Cloud189Client, cloud189_service};
use axum::http::StatusCode;
use serde_json::{Value, json};
use std::sync::{Arc, atomic::AtomicBool};

pub(super) async fn cloud189(
    state: AppState,
    params: Params,
    revision: u64,
    account: Arc<Value>,
    rejected: Arc<AtomicBool>,
) -> ParseOutcome {
    let mut credentials = account["cloud189"].clone();
    let token = if params.get("c189_token").is_empty() {
        params.get("p189_token")
    } else {
        params.get("c189_token")
    };
    if !token.is_empty() {
        credentials["open_access_token"] = token.into();
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
        let value = json!({"code":401,"msg":"天翼云盘凭据未配置","success":false,"data":null});
        return if params.get("type") == "down" {
            ParseOutcome::Direct(Reply::error(StatusCode::BAD_GATEWAY, "天翼云盘凭据未配置"))
        } else {
            ParseOutcome::Parsed(value, None)
        };
    }
    let client = Cloud189Client::new(state.http.clone(), credentials)
        .with_limit(state.parse_settings.upstream())
        .with_auth_rejection(rejected);
    if params.get("type") == "down" && !params.file_id().is_empty() {
        if directory(&state, &params, revision, params.get("sid")) {
            return ParseOutcome::Direct(Reply::error(StatusCode::BAD_REQUEST, "目录无法下载"));
        }
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
            Err(value) => return ParseOutcome::Direct(Reply::fast_error(value)),
        };
        // Return the supplier URL directly. The client follows any CDN hop;
        // this server never issues Range GET or receives file/video bytes.
        return ParseOutcome::Direct(Reply::redirect(url));
    }
    let (value, index) = match params.get("action") {
        "parse_cloud189_file" => (
            cloud189_service::parse_file(
                client,
                params.get("share_id"),
                params.get("file_id"),
                params.get("file_name"),
                params.get("file_size"),
                params.get("pwd"),
                params.get("share_code"),
            )
            .await,
            None,
        ),
        "parse_cloud189_folder_item" => (
            cloud189_service::browse_folder_item(
                client,
                params.get("share_id"),
                params.get("folder_id"),
                params.get("target_file_id"),
                params.get("pwd"),
                if params.get("share_mode").is_empty() {
                    "1"
                } else {
                    params.get("share_mode")
                },
                params.get("share_code"),
            )
            .await,
            None,
        ),
        _ => {
            cloud189_service::parse_indexed(
                client,
                params.get("url"),
                params.get("pwd"),
                &params.folders(),
                params.file_id(),
            )
            .await
        }
    };
    ParseOutcome::Parsed(value, index)
}
