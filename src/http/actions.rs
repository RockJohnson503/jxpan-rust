//! Record, statistics, and settings actions shared by page entrypoints.
use super::{AppState, params::Params, presentation::unsupported, reply::Reply};
use axum::{http::StatusCode, response::Response};
use serde_json::{Value, json};

pub(super) async fn records_action(state: &AppState, params: &Params) -> Response {
    use crate::storage::StorageError;
    let page_num = number(params.get("page"), 1);
    let limit = number(params.get("limit"), 10).min(50);
    let outcome: Result<Value, StorageError> = match params.get("action") {
        "get_stats" => state.records.stats().await,
        "get_log_settings" => state
            .records
            .retention_days()
            .await
            .map(|days| json!({"log_retention_days":days})),
        "get_parse_settings" => Ok(state.parse_settings.to_json()),
        "set_parse_settings" => match params.get("concurrency").parse::<usize>() {
            Ok(concurrency) if (1..=crate::settings::MAX_CONCURRENCY).contains(&concurrency) => {
                state
                    .parse_settings
                    .set_concurrency(concurrency)
                    .await
                    .map(|()| state.parse_settings.to_json())
            }
            _ => {
                return Reply::error(StatusCode::BAD_REQUEST, "上游并发数必须是 1–16 的整数")
                    .response();
            }
        },
        "set_log_settings" => match params.get("log_retention_days").parse::<u16>() {
            Ok(days) if (1..=3650).contains(&days) => state
                .records
                .set_retention_days(days)
                .await
                .map(|()| json!({"log_retention_days":days})),
            _ => {
                return Reply::error(StatusCode::BAD_REQUEST, "日志保留天数必须是 1–3650 的整数")
                    .response();
            }
        },
        "get_records" | "search_records" => {
            if params.get("action") == "search_records" && params.get("search").trim().is_empty() {
                return Reply::error(StatusCode::BAD_REQUEST, "缺少搜索参数").response();
            }
            state
                .records
                .list(page_num, limit, params.get("type"), params.get("search"))
                .await
        }
        "get_success_records" | "get_failed_records" => {
            let kind = if params.get("action") == "get_success_records" {
                "success"
            } else {
                "failed"
            };
            state.records.list(page_num,limit,kind,params.get("search")).await.map(|data|json!({"records":data[kind],"total":data["total"],"current_page":page_num,"per_page":limit,"total_pages":data["pages"],"has_prev":page_num>1,"has_next":u64::from(page_num)<data["pages"].as_u64().unwrap_or(1)}))
        }
        "delete_record" => match params.get("record_id").parse::<i64>() {
            Ok(id) if id > 0 => state
                .records
                .delete(id)
                .await
                .map(|deleted| json!({"deleted":deleted})),
            _ => return Reply::error(StatusCode::BAD_REQUEST, "缺少或无效的 record_id").response(),
        },
        "clear_all_records" => state
            .records
            .clear()
            .await
            .map(|count| json!({"deleted_count":count})),
        "delete_batch_records" => {
            match state.records.delete_batch(number(params.get("limit"),10).clamp(1,10)).await {
                Ok(count)=>state.records.count().await.map(|remaining|json!({"deleted_count":count,"remaining":remaining,"finished":remaining==0})),
                Err(e)=>Err(e),
            }
        }
        "count_records" => state
            .records
            .count()
            .await
            .map(|total| json!({"total":total})),
        "cancel_clear" => Ok(json!({"cancelled":true})),
        _ => return unsupported(),
    };
    match outcome {
        Ok(data) => {
            let mut value = json!({"code":200,"msg":"操作成功","success":true,"data":data});
            // Preserve the old clear-loop top-level fields used by the imported page.
            if matches!(
                params.get("action"),
                "clear_all_records" | "delete_batch_records" | "count_records"
            ) && let Some(object) = value["data"].as_object().cloned()
            {
                value.as_object_mut().unwrap().extend(object);
            }
            Reply::json(value).response()
        }
        Err(StorageError::InvalidJson) => Reply::error(
            StatusCode::BAD_REQUEST,
            "筛选参数无效或记录页过大，请缩小范围",
        )
        .response(),
        Err(_) => Reply::error(StatusCode::SERVICE_UNAVAILABLE, "记录存储操作失败").response(),
    }
}
fn number(input: &str, default: u32) -> u32 {
    input
        .parse::<u32>()
        .ok()
        .filter(|v| *v > 0)
        .unwrap_or(default)
}
