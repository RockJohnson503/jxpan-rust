//! Mobile provider request adaptation; parse lifecycle remains in the parent.
use super::{AppState, Params, ParseOutcome, Reply, directory};
use crate::providers::{
    mcloud,
    mcloud_client::MobileCloudClient,
    mcloud_service::{self, ServiceConfig},
};
use axum::http::StatusCode;
use serde_json::{Value, json};
use std::sync::{Arc, atomic::AtomicBool};

pub(super) async fn mobile(
    state: AppState,
    params: Params,
    revision: u64,
    credentials: Arc<Value>,
    rejected: Arc<AtomicBool>,
) -> ParseOutcome {
    let config = &credentials["mcloud"];
    let authorization = config["authorization"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or(params.get("auth"))
        .to_owned();
    let configured = !authorization.is_empty();
    let enabled = config["enabled"] != false && config["enabled"] != "false";
    let client = MobileCloudClient::new(state.http.clone(), authorization)
        .with_limit(state.parse_settings.upstream())
        .with_auth_rejection(rejected);
    if params.get("type") == "down" && !params.file_id().is_empty() {
        if !enabled {
            return ParseOutcome::Direct(Reply::error(
                StatusCode::SERVICE_UNAVAILABLE,
                "移动云盘解析已禁用",
            ));
        }
        if !configured {
            return ParseOutcome::Direct(Reply::error(
                StatusCode::BAD_GATEWAY,
                "移动云盘凭据未配置",
            ));
        }
        let sid = if params.get("sid").is_empty() {
            mcloud::extract_share_info(params.get("url")).unwrap_or_default()
        } else {
            params.get("sid").to_owned()
        };
        if sid.is_empty() {
            return ParseOutcome::Direct(Reply::error(
                StatusCode::BAD_REQUEST,
                "缺少或无效的分享 ID",
            ));
        }
        if directory(&state, &params, revision, &sid) {
            return ParseOutcome::Direct(Reply::error(StatusCode::BAD_REQUEST, "目录无法下载"));
        }
        return ParseOutcome::Direct(match client.download_url(&sid, params.file_id()).await {
            Ok(Some(url)) => Reply::redirect(url),
            _ => Reply::error(StatusCode::BAD_GATEWAY, "移动云盘获取下载链接失败"),
        });
    }
    let (value, index) = if params.get("action") == "parse_mcloud_subfolder" {
        if !enabled || !configured {
            (
                json!({"code":if enabled {401} else {503},"msg":"移动云盘未配置或已禁用","success":false,"data":null}),
                None,
            )
        } else if !params.get("file_id").is_empty() || !params.file_id().is_empty() {
            // The legacy page sends file_id; resolve a single/root or nested target
            // through the existing bounded file lookup, not an empty fid-chain.
            mcloud_service::parse_indexed(
                &client,
                ServiceConfig {
                    enabled,
                    authorization_configured: configured,
                },
                params.get("url"),
                params.get("pwd"),
                &params.folders(),
                if params.get("file_id").is_empty() {
                    params.file_id()
                } else {
                    params.get("file_id")
                },
            )
            .await
        } else if let Some(link) = mcloud::extract_share_info(params.get("url")) {
            (
                mcloud_service::parse_with_fid_chain(
                    &client,
                    &link,
                    params.get("pwd"),
                    &params.folders(),
                    params.file_id(),
                )
                .await,
                None,
            )
        } else {
            (
                json!({"code":400,"msg":"缺少或无效的分享链接参数","success":false,"data":null}),
                None,
            )
        }
    } else {
        mcloud_service::parse_indexed(
            &client,
            ServiceConfig {
                enabled,
                authorization_configured: configured,
            },
            params.get("url"),
            params.get("pwd"),
            &params.folders(),
            params.file_id(),
        )
        .await
    };
    ParseOutcome::Parsed(value, index)
}
